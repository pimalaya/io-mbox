//! I/O-free coroutine bringing an [`MboxIndex`] up to date with its file.
//!
//! # Example
//!
//! ```rust,no_run
//! use io_mbox::{client::MboxClient, index::sync::*};
//!
//! let client = MboxClient::new("/path/to/mail");
//! let path = client.store.resolve(&"archive".into());
//! let coroutine = MboxIndexSync::new(path, None, Default::default());
//! let output = client.run(coroutine).unwrap();
//!
//! println!("{} messages", output.index.messages().count());
//! ```

use core::{fmt, mem};

use alloc::{boxed::Box, vec::Vec};

use log::{debug, trace};
use thiserror::Error;

use crate::{
    coroutine::*,
    entry::MboxEntry,
    index::{MboxIndex, tail_hash, tail_range},
    path::MboxFsPath,
    scan::{MboxScanner, MboxScannerOptions},
};

/// Default size of a read.
pub(crate) const CHUNK_SIZE: usize = 64 * 1024;

/// Failure causes of a [`MboxIndexSync`] step.
#[derive(Clone, Debug, Error)]
pub enum MboxIndexSyncError {
    /// A reply arrived that does not match the awaited step.
    #[error("Mbox index sync failed: unexpected arg {0:?}")]
    UnexpectedArg(Option<MboxReply>),
    /// The file does not exist.
    #[error("Mbox index sync failed: {0} not found")]
    NotFound(MboxFsPath),
}

/// Options of a [`MboxIndexSync`].
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct MboxIndexSyncOptions {
    /// Scanner options. An index produced with other options is rebuilt.
    pub scanner: MboxScannerOptions,
    /// Size of a read, 64 KiB when unset.
    pub chunk_size: Option<usize>,
}

/// What [`MboxIndexSync`] did to the index it was given.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MboxIndexSyncOutcome {
    /// The file was unchanged, the index was reused as is.
    Reused,
    /// The file only grew, the new tail was scanned.
    Resumed,
    /// The whole file was scanned.
    Rebuilt,
}

/// Result of a [`MboxIndexSync`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MboxIndexSyncOutput {
    /// The up-to-date index.
    pub index: MboxIndex,
    /// How it was obtained.
    pub outcome: MboxIndexSyncOutcome,
}

/// Brings an [`MboxIndex`] up to date with its file, scanning as little
/// as the file changes allow.
#[derive(Debug)]
pub struct MboxIndexSync {
    path: MboxFsPath,
    index: Option<MboxIndex>,
    opts: MboxIndexSyncOptions,
    state: State,
}

impl MboxIndexSync {
    /// Builds a coroutine syncing `index`, if any, with the file at
    /// `path`.
    pub fn new(path: MboxFsPath, index: Option<MboxIndex>, opts: MboxIndexSyncOptions) -> Self {
        Self {
            path,
            index,
            opts,
            state: State::Stat,
        }
    }

    fn chunk_size(&self) -> usize {
        self.opts.chunk_size.unwrap_or(CHUNK_SIZE).max(1)
    }

    /// Starts a scan from the last entry of a still-valid index, or from
    /// the start of the file.
    fn scan(
        &mut self,
        meta: MboxFileMeta,
        resume: Option<MboxIndex>,
    ) -> MboxCoroutineState<MboxYield, <Self as MboxCoroutine>::Return> {
        let (entries, scanner, outcome) = match resume {
            Some(mut index) if !index.entries.is_empty() => {
                let last = index.entries.pop().map(|entry| entry.offset).unwrap_or(0);
                let mut scanner = MboxScanner::new(last, self.opts.scanner.clone());
                scanner.seed(index.entries.iter().map(|entry| entry.id.as_str()));
                debug!("resume mbox scan");
                trace!("offset: {last}");
                (index.entries, scanner, MboxIndexSyncOutcome::Resumed)
            }
            _ => {
                debug!("scan whole mbox");
                let scanner = MboxScanner::new(0, self.opts.scanner.clone());
                (Vec::new(), scanner, MboxIndexSyncOutcome::Rebuilt)
            }
        };

        self.state = State::Scan {
            meta,
            scanner: Box::new(scanner),
            entries,
            outcome,
        };
        self.step(None)
    }

    fn step(
        &mut self,
        bytes: Option<Vec<u8>>,
    ) -> MboxCoroutineState<MboxYield, <Self as MboxCoroutine>::Return> {
        let chunk_size = self.chunk_size();
        let State::Scan {
            meta,
            scanner,
            entries,
            outcome,
        } = &mut self.state
        else {
            return MboxCoroutineState::Complete(Err(MboxIndexSyncError::UnexpectedArg(None)));
        };

        let eof = match bytes {
            Some(bytes) if bytes.is_empty() => true,
            Some(bytes) => {
                entries.extend(scanner.feed(&bytes));
                false
            }
            None => false,
        };

        let offset = scanner.offset();
        if !eof && offset < meta.size {
            let len = chunk_size.min((meta.size - offset) as usize);
            return MboxCoroutineState::Yielded(MboxYield::WantsFileRead {
                path: self.path.clone(),
                offset,
                len,
            });
        }

        let scanner = mem::replace(scanner, Box::new(MboxScanner::new(0, Default::default())));
        entries.extend(scanner.finish());

        let (offset, len) = tail_range(meta.size);
        self.state = State::Tail {
            meta: *meta,
            entries: mem::take(entries),
            outcome: *outcome,
        };
        MboxCoroutineState::Yielded(MboxYield::WantsFileRead {
            path: self.path.clone(),
            offset,
            len,
        })
    }
}

impl MboxCoroutine for MboxIndexSync {
    type Yield = MboxYield;
    type Return = Result<MboxIndexSyncOutput, MboxIndexSyncError>;

    fn resume(&mut self, arg: Option<MboxReply>) -> MboxCoroutineState<Self::Yield, Self::Return> {
        match (&mut self.state, arg) {
            (State::Stat, None) => {
                self.state = State::Meta;
                MboxCoroutineState::Yielded(MboxYield::WantsFileMeta(self.path.clone()))
            }
            (State::Meta, Some(MboxReply::FileMeta(None))) => {
                let err = MboxIndexSyncError::NotFound(self.path.clone());
                MboxCoroutineState::Complete(Err(err))
            }
            (State::Meta, Some(MboxReply::FileMeta(Some(meta)))) => {
                trace!("meta: {meta:?}");
                let index = self
                    .index
                    .take()
                    .filter(|index| index.opts == self.opts.scanner);

                let Some(index) = index else {
                    return self.scan(meta, None);
                };

                if index.meta == meta {
                    debug!("mbox unchanged, reuse index");
                    let outcome = MboxIndexSyncOutcome::Reused;
                    return MboxCoroutineState::Complete(Ok(MboxIndexSyncOutput {
                        index,
                        outcome,
                    }));
                }

                if index.meta.size == 0
                    || meta.size <= index.meta.size
                    || meta.inode != index.meta.inode
                {
                    return self.scan(meta, None);
                }

                let (offset, len) = tail_range(index.meta.size);
                self.state = State::CheckTail { meta, index };
                MboxCoroutineState::Yielded(MboxYield::WantsFileRead {
                    path: self.path.clone(),
                    offset,
                    len,
                })
            }
            (State::CheckTail { meta, index }, Some(MboxReply::FileRead(bytes))) => {
                let meta = *meta;
                let index = mem::take(index);
                if tail_hash(&bytes) == index.tail {
                    self.scan(meta, Some(index))
                } else {
                    debug!("mbox tail changed, rebuild index");
                    self.scan(meta, None)
                }
            }
            (State::Scan { .. }, Some(MboxReply::FileRead(bytes))) => self.step(Some(bytes)),
            (
                State::Tail {
                    meta,
                    entries,
                    outcome,
                },
                Some(MboxReply::FileRead(bytes)),
            ) => {
                let index = MboxIndex {
                    meta: *meta,
                    tail: tail_hash(&bytes),
                    opts: self.opts.scanner.clone(),
                    entries: mem::take(entries),
                };
                debug!("mbox index synced");
                trace!("entries: {}", index.entries.len());
                let outcome = *outcome;
                MboxCoroutineState::Complete(Ok(MboxIndexSyncOutput { index, outcome }))
            }
            (_, arg) => {
                let err = MboxIndexSyncError::UnexpectedArg(arg);
                MboxCoroutineState::Complete(Err(err))
            }
        }
    }
}

#[derive(Debug)]
enum State {
    Stat,
    Meta,
    CheckTail {
        meta: MboxFileMeta,
        index: MboxIndex,
    },
    Scan {
        meta: MboxFileMeta,
        scanner: Box<MboxScanner>,
        entries: Vec<MboxEntry>,
        outcome: MboxIndexSyncOutcome,
    },
    Tail {
        meta: MboxFileMeta,
        entries: Vec<MboxEntry>,
        outcome: MboxIndexSyncOutcome,
    },
}

impl fmt::Display for State {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Stat => f.write_str("stat file"),
            Self::Meta => f.write_str("read meta"),
            Self::CheckTail { .. } => f.write_str("check tail"),
            Self::Scan { .. } => f.write_str("scan file"),
            Self::Tail { .. } => f.write_str("hash tail"),
        }
    }
}

#[cfg(test)]
mod tests {
    use alloc::vec::Vec;

    use crate::{coroutine::*, index::sync::*, path::MboxFsPath};

    const ONE: &[u8] = b"From a@b Mon Jan  1 00:00:00 2024\nSubject: one\n\nbody\n\n";
    const TWO: &[u8] = b"From c@d Mon Jan  1 00:00:00 2024\nSubject: two\n\nbody\n\n";

    fn meta(size: usize, inode: u64) -> MboxFileMeta {
        MboxFileMeta {
            size: size as u64,
            inode,
            ..Default::default()
        }
    }

    /// Runs a sync against `file`, answering every read from it.
    fn run(
        file: &[u8],
        meta: Option<MboxFileMeta>,
        index: Option<MboxIndex>,
        chunk: usize,
    ) -> Result<MboxIndexSyncOutput, MboxIndexSyncError> {
        let opts = MboxIndexSyncOptions {
            chunk_size: Some(chunk),
            ..Default::default()
        };
        let mut cor = MboxIndexSync::new(MboxFsPath::new("mbox"), index, opts);
        let mut arg = None;
        loop {
            match cor.resume(arg.take()) {
                MboxCoroutineState::Complete(result) => return result,
                MboxCoroutineState::Yielded(MboxYield::WantsFileMeta(_)) => {
                    arg = Some(MboxReply::FileMeta(meta));
                }
                MboxCoroutineState::Yielded(MboxYield::WantsFileRead { offset, len, .. }) => {
                    let start = (offset as usize).min(file.len());
                    let end = (start + len).min(file.len());
                    arg = Some(MboxReply::FileRead(file[start..end].to_vec()));
                }
                state => panic!("unexpected {state:?}"),
            }
        }
    }

    #[test]
    fn scans_then_reuses() {
        let out = run(ONE, Some(meta(ONE.len(), 1)), None, 7).unwrap();
        assert_eq!(out.outcome, MboxIndexSyncOutcome::Rebuilt);
        assert_eq!(out.index.entries.len(), 1);

        let again = run(ONE, Some(meta(ONE.len(), 1)), Some(out.index.clone()), 7).unwrap();
        assert_eq!(again.outcome, MboxIndexSyncOutcome::Reused);
        assert_eq!(again.index, out.index);
    }

    #[test]
    fn appended_file_resumes() {
        let first = run(ONE, Some(meta(ONE.len(), 1)), None, 7).unwrap().index;
        let mut grown: Vec<u8> = ONE.to_vec();
        grown.extend_from_slice(TWO);
        grown.extend_from_slice(ONE);

        let out = run(&grown, Some(meta(grown.len(), 1)), Some(first), 5).unwrap();
        let full = run(&grown, Some(meta(grown.len(), 1)), None, 5).unwrap();
        assert_eq!(out.outcome, MboxIndexSyncOutcome::Resumed);
        assert_eq!(out.index, full.index);
        assert!(out.index.entries[2].id.ends_with("-2"));
    }

    #[test]
    fn rewritten_file_rebuilds() {
        let first = run(ONE, Some(meta(ONE.len(), 1)), None, 7).unwrap().index;

        let mut other: Vec<u8> = TWO.to_vec();
        other.extend_from_slice(TWO);
        let out = run(&other, Some(meta(other.len(), 1)), Some(first.clone()), 7).unwrap();
        assert_eq!(out.outcome, MboxIndexSyncOutcome::Rebuilt);

        let out = run(&other, Some(meta(other.len(), 2)), Some(first.clone()), 7).unwrap();
        assert_eq!(out.outcome, MboxIndexSyncOutcome::Rebuilt);

        let out = run(b"", Some(meta(0, 1)), Some(first.clone()), 7).unwrap();
        assert_eq!(out.outcome, MboxIndexSyncOutcome::Rebuilt);
        assert!(out.index.entries.is_empty());

        let mut first = first;
        first.opts.header = true;
        let out = run(ONE, Some(meta(ONE.len(), 1)), Some(first), 7).unwrap();
        assert_eq!(out.outcome, MboxIndexSyncOutcome::Rebuilt);
    }

    #[test]
    fn shrinking_reads_stop_at_eof() {
        let out = run(&ONE[..10], Some(meta(ONE.len(), 1)), None, 4).unwrap();
        assert!(out.index.entries.is_empty());
    }

    #[test]
    fn missing_file_and_bad_reply() {
        let err = run(ONE, None, None, 7).unwrap_err();
        assert!(matches!(err, MboxIndexSyncError::NotFound(_)));

        let mut cor = MboxIndexSync::new(MboxFsPath::new("x"), None, Default::default());
        cor.resume(None);
        let state = cor.resume(Some(MboxReply::Sleep));
        assert!(matches!(
            state,
            MboxCoroutineState::Complete(Err(MboxIndexSyncError::UnexpectedArg(_)))
        ));
    }
}
