//! I/O-free coroutine rewriting the flags of messages and removing
//! messages from an mbox.
//!
//! mbox has no room for in-place edits: a longer `Status` field shifts
//! every byte after it. The rewrite follows mutt. Under lock, the index
//! is synced and the new layout of the file from the first edited
//! message onwards is written to a temporary file: unchanged ranges
//! copied, flag fields swapped, removed messages skipped. That tail is
//! then copied back over the original at the same offset, and the file
//! truncated and synced. The mbox keeps its inode, owner and mode, and
//! the directory holding it needs no write access (a spool usually
//! grants none).
//!
//! The temporary file is removed once copied back. A failure while
//! copying back leaves it in place: it then holds the only intact copy
//! of the tail, which the std client names in its error.
//!
//! Flag fields are located by [`crate::entry::MboxEntry::flag_spans`], so
//! no message is parsed again, and content ids do not change.
//!
//! # Example
//!
//! ```rust,no_run
//! use std::collections::BTreeMap;
//!
//! use io_mbox::{client::MboxClient, entry::update::MboxEntryUpdateEdit};
//!
//! let client = MboxClient::new("/path/to/mail");
//! let index = client.sync_index("archive", None).unwrap().index;
//! let id = index.messages().next().unwrap().id.clone();
//! let edits = BTreeMap::from([(id, MboxEntryUpdateEdit::Remove)]);
//! let output = client.update("archive", edits, Some(index)).unwrap();
//!
//! println!("{} removed", output.removed);
//! ```

use core::{fmt, mem};

use alloc::{
    boxed::Box,
    collections::BTreeMap,
    string::{String, ToString},
    vec::Vec,
};

use log::{debug, trace};
use thiserror::Error;

use crate::{
    coroutine::*,
    entry::{MboxEntry, MboxSpan},
    flag::MboxFlags,
    index::{
        MboxIndex,
        sync::{CHUNK_SIZE, MboxIndexSync, MboxIndexSyncError, MboxIndexSyncOptions},
        tail_hash, tail_range,
    },
    lock::{MboxLock, MboxLockOptions, acquire::*, release::*},
    mbox_try,
    path::MboxFsPath,
    range::{MboxRange, MboxRangeCopy, MboxRangeCopyError},
    scan::MboxScannerOptions,
};

/// Failure causes of a [`MboxEntryUpdate`] step.
#[derive(Clone, Debug, Error)]
pub enum MboxEntryUpdateError {
    /// A reply arrived that does not match the awaited step.
    #[error("Mbox message update failed: unexpected arg {0:?}")]
    UnexpectedArg(Option<MboxReply>),
    /// The mbox does not exist.
    #[error("Mbox message update failed: {0} not found")]
    NotFound(MboxFsPath),
    /// No message has this id.
    #[error("Mbox message update failed: message {0} not found")]
    MessageNotFound(String),
    /// The locks could not be taken.
    #[error(transparent)]
    Lock(#[from] MboxLockAcquireError),
    /// The locks could not be released.
    #[error(transparent)]
    Unlock(#[from] MboxLockReleaseError),
    /// The index could not be synced.
    #[error(transparent)]
    Sync(#[from] MboxIndexSyncError),
    /// A range could not be copied.
    #[error(transparent)]
    Copy(#[from] MboxRangeCopyError),
}

/// What to do to one message.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MboxEntryUpdateEdit {
    /// Replace its flags.
    Flags(MboxFlags),
    /// Remove it from the file.
    Remove,
}

/// Options of a [`MboxEntryUpdate`].
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct MboxEntryUpdateOptions {
    /// Index of the file, synced under lock before use. Unset, the whole
    /// file is scanned.
    pub index: Option<MboxIndex>,
    /// Locks to take.
    pub lock: MboxLockOptions,
    /// Scanner options the index is produced with.
    pub scanner: MboxScannerOptions,
    /// Size of a read, 64 KiB when unset.
    pub chunk_size: Option<usize>,
}

/// Result of a [`MboxEntryUpdate`].
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct MboxEntryUpdateOutput {
    /// Index of the rewritten file.
    pub index: MboxIndex,
    /// How many messages got new flags.
    pub updated: usize,
    /// How many messages were removed.
    pub removed: usize,
}

/// Rewrites flags and removes messages of an mbox, under lock.
#[derive(Debug)]
pub struct MboxEntryUpdate {
    path: MboxFsPath,
    edits: BTreeMap<String, MboxEntryUpdateEdit>,
    opts: MboxEntryUpdateOptions,
    lock: Option<MboxLock>,
    index: MboxIndex,
    plan: Plan,
    temp: Option<MboxFsPath>,
    /// Whether the temporary file started to be copied back, after which
    /// it is the copy to recover from and must stay.
    copying_back: bool,
    result: Option<Result<MboxEntryUpdateOutput, MboxEntryUpdateError>>,
    state: State,
}

impl MboxEntryUpdate {
    /// Builds a coroutine applying `edits`, keyed by message id, to the
    /// mbox at `path`.
    pub fn new(
        path: MboxFsPath,
        edits: BTreeMap<String, MboxEntryUpdateEdit>,
        opts: MboxEntryUpdateOptions,
    ) -> Self {
        Self {
            path,
            edits,
            opts,
            lock: None,
            index: MboxIndex::default(),
            plan: Plan::default(),
            temp: None,
            copying_back: false,
            result: None,
            state: State::Check,
        }
    }

    fn chunk_size(&self) -> usize {
        self.opts.chunk_size.unwrap_or(CHUNK_SIZE).max(1)
    }

    /// Removes the temporary file unless it is being copied back, then
    /// releases the locks and completes with `result`.
    fn finish(
        &mut self,
        result: Result<MboxEntryUpdateOutput, MboxEntryUpdateError>,
    ) -> MboxCoroutineState<MboxYield, <Self as MboxCoroutine>::Return> {
        self.result = Some(result);

        if !self.copying_back
            && let Some(temp) = self.temp.take()
        {
            self.state = State::Cleanup;
            return MboxCoroutineState::Yielded(MboxYield::WantsFileRemove(temp));
        }

        self.release()
    }

    fn release(&mut self) -> MboxCoroutineState<MboxYield, <Self as MboxCoroutine>::Return> {
        let Some(lock) = self.lock.take() else {
            self.state = State::Done;
            let result = self.result.take().unwrap_or_else(|| Ok(Default::default()));
            return MboxCoroutineState::Complete(result);
        };
        self.state = State::Release(MboxLockRelease::new(lock));
        self.resume(None)
    }

    /// Plans the rewrite once the index is synced and the last byte of the
    /// file is known.
    fn plan(
        &mut self,
        ends_with_newline: bool,
    ) -> MboxCoroutineState<MboxYield, <Self as MboxCoroutine>::Return> {
        let mut edits = BTreeMap::new();
        for (id, edit) in mem::take(&mut self.edits) {
            let Some(pos) = self.index.entries.iter().position(|entry| entry.id == id) else {
                return self.finish(Err(MboxEntryUpdateError::MessageNotFound(id)));
            };
            if let MboxEntryUpdateEdit::Flags(flags) = &edit
                && *flags == self.index.entries[pos].flags
            {
                continue;
            }
            edits.insert(pos, edit);
        }

        let Some(first) = edits.keys().next().copied() else {
            debug!("mbox already up to date");
            let output = MboxEntryUpdateOutput {
                index: mem::take(&mut self.index),
                ..Default::default()
            };
            return self.finish(Ok(output));
        };

        self.plan = Plan::new(&self.index, first, &edits, ends_with_newline);
        trace!("rewrite from: {}", self.plan.start);
        trace!("rewrite len: {}", self.plan.len);

        self.state = State::Temp;
        MboxCoroutineState::Yielded(MboxYield::WantsTempFile)
    }

    /// Writes the next piece of the plan to the temporary file.
    fn fill(&mut self) -> MboxCoroutineState<MboxYield, <Self as MboxCoroutine>::Return> {
        let Some(temp) = self.temp.clone() else {
            return self.finish(Err(MboxEntryUpdateError::UnexpectedArg(None)));
        };

        let Some(piece) = self.plan.pieces.pop() else {
            self.state = State::SyncTemp;
            return MboxCoroutineState::Yielded(MboxYield::WantsFileSync(temp));
        };

        let offset = self.plan.written;
        match piece {
            Piece::Bytes(bytes) => {
                self.plan.written += bytes.len() as u64;
                self.state = State::Fill(None);
                MboxCoroutineState::Yielded(MboxYield::WantsFileWrite {
                    path: temp,
                    offset,
                    bytes,
                })
            }
            Piece::Copy { offset: src, len } => {
                self.plan.written += len;
                let src = MboxRange {
                    path: self.path.clone(),
                    offset: src,
                };
                let dst = MboxRange { path: temp, offset };
                let copy = MboxRangeCopy::new(src, dst, len, self.chunk_size());
                self.state = State::Fill(Some(copy));
                self.resume(None)
            }
        }
    }
}

impl MboxCoroutine for MboxEntryUpdate {
    type Yield = MboxYield;
    type Return = Result<MboxEntryUpdateOutput, MboxEntryUpdateError>;

    fn resume(&mut self, arg: Option<MboxReply>) -> MboxCoroutineState<Self::Yield, Self::Return> {
        match (&mut self.state, arg) {
            (State::Check, None) => {
                self.state = State::Checking;
                MboxCoroutineState::Yielded(MboxYield::WantsFileMeta(self.path.clone()))
            }
            (State::Checking, Some(MboxReply::FileMeta(None))) => {
                let err = MboxEntryUpdateError::NotFound(self.path.clone());
                self.finish(Err(err))
            }
            (State::Checking, Some(MboxReply::FileMeta(Some(_)))) => {
                let acquire = MboxLockAcquire::new(self.path.clone(), self.opts.lock.clone());
                self.state = State::Lock(acquire);
                self.resume(None)
            }
            (State::Lock(acquire), arg) => {
                self.lock = Some(mbox_try!(acquire, arg));
                let opts = MboxIndexSyncOptions {
                    scanner: self.opts.scanner.clone(),
                    chunk_size: self.opts.chunk_size,
                };
                let index = self.opts.index.take();
                let sync = MboxIndexSync::new(self.path.clone(), index, opts);
                self.state = State::Sync(Box::new(sync));
                self.resume(None)
            }
            (State::Sync(sync), arg) => {
                let output = match sync.resume(arg) {
                    MboxCoroutineState::Yielded(y) => return MboxCoroutineState::Yielded(y),
                    MboxCoroutineState::Complete(Err(err)) => return self.finish(Err(err.into())),
                    MboxCoroutineState::Complete(Ok(output)) => output,
                };
                self.index = output.index;
                let size = self.index.meta.size;
                if size == 0 {
                    return self.plan(true);
                }
                self.state = State::LastByte;
                MboxCoroutineState::Yielded(MboxYield::WantsFileRead {
                    path: self.path.clone(),
                    offset: size - 1,
                    len: 1,
                })
            }
            (State::LastByte, Some(MboxReply::FileRead(bytes))) => {
                let ends_with_newline = bytes.last() == Some(&b'\n');
                self.plan(ends_with_newline)
            }
            (State::Temp, Some(MboxReply::TempFile(temp))) => {
                trace!("temp: {temp}");
                self.temp = Some(temp);
                self.fill()
            }
            (State::Fill(None), Some(MboxReply::FileWrite)) => self.fill(),
            (State::Fill(Some(copy)), arg) => {
                let result = copy.resume(arg);
                match result {
                    MboxCoroutineState::Yielded(y) => MboxCoroutineState::Yielded(y),
                    MboxCoroutineState::Complete(Err(err)) => self.finish(Err(err.into())),
                    MboxCoroutineState::Complete(Ok(())) => self.fill(),
                }
            }
            (State::SyncTemp, Some(MboxReply::FileSync)) => {
                let Some(temp) = self.temp.clone() else {
                    return self.finish(Err(MboxEntryUpdateError::UnexpectedArg(None)));
                };
                self.copying_back = true;
                let src = MboxRange {
                    path: temp,
                    offset: 0,
                };
                let dst = MboxRange {
                    path: self.path.clone(),
                    offset: self.plan.start,
                };
                let copy = MboxRangeCopy::new(src, dst, self.plan.len, self.chunk_size());
                self.state = State::CopyBack(copy);
                self.resume(None)
            }
            (State::CopyBack(copy), arg) => {
                match copy.resume(arg) {
                    MboxCoroutineState::Yielded(y) => return MboxCoroutineState::Yielded(y),
                    MboxCoroutineState::Complete(Err(err)) => return self.finish(Err(err.into())),
                    MboxCoroutineState::Complete(Ok(())) => (),
                }
                self.state = State::Truncate;
                MboxCoroutineState::Yielded(MboxYield::WantsFileTruncate {
                    path: self.path.clone(),
                    len: self.plan.start + self.plan.len,
                })
            }
            (State::Truncate, Some(MboxReply::FileTruncate)) => {
                self.state = State::SyncFile;
                MboxCoroutineState::Yielded(MboxYield::WantsFileSync(self.path.clone()))
            }
            (State::SyncFile, Some(MboxReply::FileSync)) => {
                self.copying_back = false;
                self.state = State::RemoveTemp;
                match self.temp.take() {
                    Some(temp) => MboxCoroutineState::Yielded(MboxYield::WantsFileRemove(temp)),
                    None => self.resume(Some(MboxReply::FileRemove)),
                }
            }
            (State::RemoveTemp, Some(MboxReply::FileRemove)) => {
                self.state = State::Stat;
                MboxCoroutineState::Yielded(MboxYield::WantsFileMeta(self.path.clone()))
            }
            (State::Stat, Some(MboxReply::FileMeta(Some(meta)))) => {
                self.index.meta = meta;
                let (offset, len) = tail_range(meta.size);
                self.state = State::Tail;
                MboxCoroutineState::Yielded(MboxYield::WantsFileRead {
                    path: self.path.clone(),
                    offset,
                    len,
                })
            }
            (State::Tail, Some(MboxReply::FileRead(bytes))) => {
                self.index.tail = tail_hash(&bytes);
                self.index.entries = mem::take(&mut self.plan.entries);
                debug!("mbox rewritten");
                trace!("updated: {}", self.plan.updated);
                trace!("removed: {}", self.plan.removed);
                let output = MboxEntryUpdateOutput {
                    index: mem::take(&mut self.index),
                    updated: self.plan.updated,
                    removed: self.plan.removed,
                };
                self.finish(Ok(output))
            }
            (State::Cleanup, Some(MboxReply::FileRemove)) => self.release(),
            (State::Release(release), arg) => {
                mbox_try!(release, arg);
                self.state = State::Done;
                let result = self.result.take().unwrap_or_else(|| Ok(Default::default()));
                MboxCoroutineState::Complete(result)
            }
            (State::Done, arg) => {
                MboxCoroutineState::Complete(Err(MboxEntryUpdateError::UnexpectedArg(arg)))
            }
            (_, arg) => self.finish(Err(MboxEntryUpdateError::UnexpectedArg(arg))),
        }
    }
}

/// The rewritten tail of the file.
#[derive(Debug, Default)]
struct Plan {
    /// Where the tail starts, the offset of the first edited message.
    start: u64,
    /// Length of the new tail.
    len: u64,
    /// What to write, in reverse order so the next one pops.
    pieces: Vec<Piece>,
    /// Bytes of the tail written so far.
    written: u64,
    /// The entries of the file once rewritten.
    entries: Vec<MboxEntry>,
    updated: usize,
    removed: usize,
}

impl Plan {
    fn new(
        index: &MboxIndex,
        first: usize,
        edits: &BTreeMap<usize, MboxEntryUpdateEdit>,
        ends_with_newline: bool,
    ) -> Self {
        let start = index.entries[first].offset;
        let mut plan = Plan {
            start,
            entries: index.entries[..first].to_vec(),
            ..Default::default()
        };
        let mut pieces = Vec::new();
        let last = index.entries.len() - 1;

        for (i, entry) in index.entries.iter().enumerate().skip(first) {
            let next = index.next_offset(i);
            let offset = start + plan.len;

            match edits.get(&i) {
                Some(MboxEntryUpdateEdit::Remove) => {
                    plan.removed += 1;
                }
                Some(MboxEntryUpdateEdit::Flags(flags)) => {
                    plan.updated += 1;
                    let header_end = entry.message_offset + entry.header_len;

                    let mut cursor = entry.offset;
                    let mut removed = 0;
                    for span in &entry.flag_spans {
                        let span_start = entry.message_offset + span.offset;
                        push_copy(&mut pieces, cursor, span_start - cursor);
                        cursor = span_start + span.len;
                        removed += span.len;
                    }
                    push_copy(&mut pieces, cursor, header_end - cursor);

                    let last_line_kept = entry
                        .flag_spans
                        .last()
                        .is_none_or(|span| span.offset + span.len < entry.header_len);
                    let unterminated = i == last
                        && entry.header_len == entry.len
                        && entry.header_len > 0
                        && !ends_with_newline
                        && last_line_kept;
                    let eol: &[u8] = if entry.crlf { b"\r\n" } else { b"\n" };

                    let mut lines = Vec::new();
                    if unterminated {
                        lines.extend_from_slice(eol);
                    }
                    let prefix = lines.len() as u64;
                    lines.extend(flags.to_header(entry.crlf));
                    let added = lines.len() as u64;
                    let flag_len = added - prefix;
                    if added > 0 {
                        pieces.push(Piece::Bytes(lines.clone()));
                    }
                    push_copy(&mut pieces, header_end, next - header_end);

                    let header_len = entry.header_len - removed + added;
                    let mut header = strip_spans(&entry.header, &entry.flag_spans);
                    if entry.header.len() as u64 == entry.header_len {
                        header.extend_from_slice(&lines);
                    }

                    plan.entries.push(MboxEntry {
                        offset,
                        message_offset: offset + (entry.message_offset - entry.offset),
                        len: entry.len - removed + added,
                        header_len,
                        flags: flags.clone(),
                        flag_spans: match flag_len {
                            0 => Vec::new(),
                            len => vec![MboxSpan {
                                offset: entry.header_len - removed + prefix,
                                len,
                            }],
                        },
                        header,
                        ..entry.clone()
                    });
                    plan.len += next - entry.offset - removed + added;
                }
                None => {
                    push_copy(&mut pieces, entry.offset, next - entry.offset);
                    plan.entries.push(MboxEntry {
                        offset,
                        message_offset: offset + (entry.message_offset - entry.offset),
                        ..entry.clone()
                    });
                    plan.len += next - entry.offset;
                }
            }
        }

        renumber(&mut plan.entries);
        pieces.reverse();
        plan.pieces = pieces;
        plan
    }
}

/// A piece of the rewritten tail.
#[derive(Debug)]
enum Piece {
    /// Bytes copied from the original file.
    Copy { offset: u64, len: u64 },
    /// New bytes.
    Bytes(Vec<u8>),
}

/// Appends a copy piece, merged into the previous one when contiguous.
fn push_copy(pieces: &mut Vec<Piece>, offset: u64, len: u64) {
    if len == 0 {
        return;
    }
    if let Some(Piece::Copy {
        offset: prev,
        len: prev_len,
    }) = pieces.last_mut()
        && *prev + *prev_len == offset
    {
        *prev_len += len;
        return;
    }
    pieces.push(Piece::Copy { offset, len });
}

/// Removes the flag spans from a captured header.
fn strip_spans(header: &[u8], spans: &[MboxSpan]) -> Vec<u8> {
    let mut out = Vec::with_capacity(header.len());
    let mut cursor = 0;
    for span in spans {
        let start = (span.offset as usize).min(header.len());
        out.extend_from_slice(&header[cursor.min(start)..start]);
        cursor = ((span.offset + span.len) as usize).min(header.len());
    }
    out.extend_from_slice(&header[cursor.min(header.len())..]);
    out
}

/// Numbers duplicate ids again after a removal, as a fresh scan would.
fn renumber(entries: &mut [MboxEntry]) {
    let mut seen: BTreeMap<String, u32> = BTreeMap::new();
    for entry in entries {
        let hash = entry.id.split('-').next().unwrap_or_default().to_string();
        let count = seen.entry(hash.clone()).or_default();
        *count += 1;
        entry.id = match *count {
            1 => hash,
            n => format!("{hash}-{n}"),
        };
    }
}

#[derive(Debug)]
enum State {
    Check,
    Checking,
    Lock(MboxLockAcquire),
    Sync(Box<MboxIndexSync>),
    LastByte,
    Temp,
    Fill(Option<MboxRangeCopy>),
    SyncTemp,
    CopyBack(MboxRangeCopy),
    Truncate,
    SyncFile,
    RemoveTemp,
    Stat,
    Tail,
    Cleanup,
    Release(MboxLockRelease),
    Done,
}

impl fmt::Display for State {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Check => f.write_str("start"),
            Self::Checking => f.write_str("check mbox"),
            Self::Lock(_) => f.write_str("lock mbox"),
            Self::Sync(_) => f.write_str("sync index"),
            Self::LastByte => f.write_str("read last byte"),
            Self::Temp => f.write_str("create temp"),
            Self::Fill(_) => f.write_str("fill temp"),
            Self::SyncTemp => f.write_str("sync temp"),
            Self::CopyBack(_) => f.write_str("copy back"),
            Self::Truncate => f.write_str("truncate mbox"),
            Self::SyncFile => f.write_str("sync mbox"),
            Self::RemoveTemp => f.write_str("remove temp"),
            Self::Stat => f.write_str("stat mbox"),
            Self::Tail => f.write_str("hash tail"),
            Self::Cleanup => f.write_str("clean up temp"),
            Self::Release(_) => f.write_str("unlock mbox"),
            Self::Done => f.write_str("done"),
        }
    }
}

#[cfg(test)]
mod tests {
    use alloc::{collections::BTreeMap, string::String, vec::Vec};

    use crate::{
        coroutine::*, entry::update::*, flag::MboxFlag, lock::MboxLockOptions, path::MboxFsPath,
        scan::MboxScanner,
    };

    /// In-memory files, answering every yield of an update.
    #[derive(Default)]
    struct Fs {
        files: BTreeMap<String, Vec<u8>>,
        temps: usize,
    }

    impl Fs {
        fn run(
            &mut self,
            edits: BTreeMap<String, MboxEntryUpdateEdit>,
            chunk: usize,
        ) -> Result<MboxEntryUpdateOutput, MboxEntryUpdateError> {
            let opts = MboxEntryUpdateOptions {
                lock: MboxLockOptions {
                    skip_dotlock: true,
                    skip_fcntl: true,
                    ..Default::default()
                },
                chunk_size: Some(chunk),
                ..Default::default()
            };
            let mut cor = MboxEntryUpdate::new(MboxFsPath::new("mbox"), edits, opts);
            let mut arg = None;
            loop {
                arg = Some(match cor.resume(arg.take()) {
                    MboxCoroutineState::Complete(result) => return result,
                    MboxCoroutineState::Yielded(y) => self.answer(y),
                });
            }
        }

        fn answer(&mut self, y: MboxYield) -> MboxReply {
            match y {
                MboxYield::WantsFileMeta(path) => {
                    let meta = self.files.get(path.as_str()).map(|file| MboxFileMeta {
                        size: file.len() as u64,
                        ..Default::default()
                    });
                    MboxReply::FileMeta(meta)
                }
                MboxYield::WantsFileRead { path, offset, len } => {
                    let file = &self.files[path.as_str()];
                    let start = (offset as usize).min(file.len());
                    let end = (start + len).min(file.len());
                    MboxReply::FileRead(file[start..end].to_vec())
                }
                MboxYield::WantsFileWrite {
                    path,
                    offset,
                    bytes,
                } => {
                    let file = self.files.get_mut(path.as_str()).unwrap();
                    let end = offset as usize + bytes.len();
                    if file.len() < end {
                        file.resize(end, 0);
                    }
                    file[offset as usize..end].copy_from_slice(&bytes);
                    MboxReply::FileWrite
                }
                MboxYield::WantsFileTruncate { path, len } => {
                    self.files
                        .get_mut(path.as_str())
                        .unwrap()
                        .truncate(len as usize);
                    MboxReply::FileTruncate
                }
                MboxYield::WantsFileSync(_) => MboxReply::FileSync,
                MboxYield::WantsTempFile => {
                    self.temps += 1;
                    let path = format!("temp{}", self.temps);
                    self.files.insert(path.clone(), Vec::new());
                    MboxReply::TempFile(MboxFsPath::new(path))
                }
                MboxYield::WantsFileRemove(path) => {
                    self.files.remove(path.as_str());
                    MboxReply::FileRemove
                }
                y => panic!("unexpected {y:?}"),
            }
        }

        fn mbox(&self) -> &[u8] {
            &self.files["mbox"]
        }
    }

    fn memfs(bytes: &[u8]) -> Fs {
        let mut fs = Fs::default();
        fs.files.insert("mbox".into(), bytes.to_vec());
        fs
    }

    fn scan(bytes: &[u8]) -> Vec<MboxEntry> {
        let mut scanner = MboxScanner::new(0, Default::default());
        let mut entries = scanner.feed(bytes);
        entries.extend(scanner.finish());
        entries
    }

    const MBOX: &[u8] = b"\
From a@b Mon Jan  1 00:00:00 2024
Subject: one
Status: O
X-Keywords: a
 b

body one

From c@d Mon Jan  1 00:00:00 2024
Subject: two

body two

From e@f Mon Jan  1 00:00:00 2024
Subject: three

body three
";

    fn flags(list: &[MboxFlag]) -> MboxEntryUpdateEdit {
        MboxEntryUpdateEdit::Flags(list.iter().cloned().collect())
    }

    #[test]
    fn rewrites_flags_in_place() {
        let entries = scan(MBOX);
        for chunk in [1, 3, 4096] {
            let mut fs = memfs(MBOX);
            let edits = BTreeMap::from([
                (
                    entries[0].id.clone(),
                    flags(&[MboxFlag::Seen, MboxFlag::Old]),
                ),
                (
                    entries[1].id.clone(),
                    flags(&[MboxFlag::Flagged, MboxFlag::Keyword("k".into())]),
                ),
            ]);
            let out = fs.run(edits, chunk).unwrap();
            assert_eq!(out.updated, 2);
            assert_eq!(out.removed, 0);

            let expected = String::from_utf8_lossy(MBOX)
                .replace("Status: O\nX-Keywords: a\n b\n", "Status: RO\n")
                .replace(
                    "Subject: two\n",
                    "Subject: two\nX-Status: F\nX-Keywords: k\n",
                );
            assert_eq!(String::from_utf8_lossy(fs.mbox()), expected);
            assert_eq!(fs.files.len(), 1, "temp file removed");

            let fresh = scan(fs.mbox());
            assert_eq!(out.index.entries, fresh);
            assert_eq!(fresh[0].id, entries[0].id);
            assert_eq!(fresh[1].id, entries[1].id);
        }
    }

    #[test]
    fn clearing_flags_restores_the_original_bytes() {
        let original = scan(MBOX);
        let mut fs = memfs(MBOX);
        let edits = BTreeMap::from([(original[1].id.clone(), flags(&[MboxFlag::Draft]))]);
        fs.run(edits, 7).unwrap();
        assert_ne!(fs.mbox(), MBOX);
        let edits = BTreeMap::from([(original[1].id.clone(), flags(&[]))]);
        let out = fs.run(edits, 7).unwrap();
        assert_eq!(fs.mbox(), MBOX);
        assert_eq!(out.index.entries, original);
    }

    #[test]
    fn removes_messages() {
        let entries = scan(MBOX);
        let mut fs = memfs(MBOX);
        let edits = BTreeMap::from([
            (entries[0].id.clone(), MboxEntryUpdateEdit::Remove),
            (entries[2].id.clone(), MboxEntryUpdateEdit::Remove),
        ]);
        let out = fs.run(edits, 5).unwrap();
        assert_eq!(out.removed, 2);
        assert_eq!(
            fs.mbox(),
            b"From c@d Mon Jan  1 00:00:00 2024\nSubject: two\n\nbody two\n\n".as_slice()
        );
        assert_eq!(out.index.entries, scan(fs.mbox()));
    }

    #[test]
    fn removing_a_duplicate_renumbers() {
        let one = b"From a@b Mon Jan  1 00:00:00 2024\nSubject: x\n\nbody\n\n";
        let bytes = [one.as_slice(), one, one].concat();
        let entries = scan(&bytes);
        let mut fs = memfs(&bytes);
        let edits = BTreeMap::from([(entries[0].id.clone(), MboxEntryUpdateEdit::Remove)]);
        let out = fs.run(edits, 4096).unwrap();
        assert_eq!(out.index.entries, scan(fs.mbox()));
        assert_eq!(out.index.entries[1].id, entries[1].id);
    }

    #[test]
    fn unterminated_last_header() {
        let bytes = b"From a@b Mon Jan  1 00:00:00 2024\nSubject: x";
        let entries = scan(bytes);
        let mut fs = memfs(bytes);
        let edits = BTreeMap::from([(entries[0].id.clone(), flags(&[MboxFlag::Seen]))]);
        let out = fs.run(edits, 4096).unwrap();
        assert_eq!(
            fs.mbox(),
            b"From a@b Mon Jan  1 00:00:00 2024\nSubject: x\nStatus: R\n".as_slice()
        );
        assert_eq!(out.index.entries, scan(fs.mbox()));

        let bytes = b"From a@b Mon Jan  1 00:00:00 2024\nSubject: x\nStatus: R";
        let entries = scan(bytes);
        let mut fs = memfs(bytes);
        let edits = BTreeMap::from([(entries[0].id.clone(), flags(&[MboxFlag::Flagged]))]);
        let out = fs.run(edits, 4096).unwrap();
        assert_eq!(
            fs.mbox(),
            b"From a@b Mon Jan  1 00:00:00 2024\nSubject: x\nX-Status: F\n".as_slice()
        );
        assert_eq!(out.index.entries, scan(fs.mbox()));
    }

    #[test]
    fn crlf_and_captured_headers() {
        let bytes = b"From a@b Mon Jan  1 00:00:00 2024\r\nSubject: x\r\nStatus: R\r\n\r\nbody\r\n";
        let entries = scan(bytes);
        let mut fs = memfs(bytes);
        let edits = BTreeMap::from([(entries[0].id.clone(), flags(&[MboxFlag::Answered]))]);
        let out = fs.run(edits, 4096).unwrap();
        assert_eq!(
            fs.mbox(),
            b"From a@b Mon Jan  1 00:00:00 2024\r\nSubject: x\r\nX-Status: A\r\n\r\nbody\r\n"
                .as_slice()
        );
        assert_eq!(out.index.entries, scan(fs.mbox()));

        let header = b"Subject: x\nStatus: R\nTo: y\n".to_vec();
        let spans = [MboxSpan {
            offset: 11,
            len: 10,
        }];
        assert_eq!(strip_spans(&header, &spans), b"Subject: x\nTo: y\n");
    }

    #[test]
    fn no_op_edits_and_unknown_ids() {
        let entries = scan(MBOX);
        let mut fs = memfs(MBOX);
        let edits = BTreeMap::from([(
            entries[0].id.clone(),
            MboxEntryUpdateEdit::Flags(entries[0].flags.clone()),
        )]);
        let out = fs.run(edits, 4096).unwrap();
        assert_eq!((out.updated, out.removed), (0, 0));
        assert_eq!(fs.mbox(), MBOX);

        let edits = BTreeMap::from([("nope".into(), MboxEntryUpdateEdit::Remove)]);
        let err = fs.run(edits, 4096).unwrap_err();
        assert!(matches!(err, MboxEntryUpdateError::MessageNotFound(_)));

        let mut missing = Fs::default();
        let err = missing.run(BTreeMap::new(), 4096).unwrap_err();
        assert!(matches!(err, MboxEntryUpdateError::NotFound(_)));

        let mut empty = fs_empty();
        let out = empty.run(BTreeMap::new(), 4096).unwrap();
        assert!(out.index.entries.is_empty());
    }

    fn fs_empty() -> Fs {
        memfs(b"")
    }

    #[test]
    fn captured_headers_follow_rewrites() {
        let opts = MboxScannerOptions {
            header: true,
            ..Default::default()
        };
        let mut scanner = MboxScanner::new(0, opts.clone());
        let mut entries = scanner.feed(MBOX);
        entries.extend(scanner.finish());

        let mut fs = memfs(MBOX);
        let edits = BTreeMap::from([(entries[0].id.clone(), flags(&[MboxFlag::Answered]))]);
        let run_opts = MboxEntryUpdateOptions {
            lock: MboxLockOptions {
                skip_dotlock: true,
                skip_fcntl: true,
                ..Default::default()
            },
            scanner: opts.clone(),
            ..Default::default()
        };
        let mut cor = MboxEntryUpdate::new(MboxFsPath::new("mbox"), edits, run_opts);
        let mut arg = None;
        let out = loop {
            arg = Some(match cor.resume(arg.take()) {
                MboxCoroutineState::Complete(result) => break result.unwrap(),
                MboxCoroutineState::Yielded(y) => fs.answer(y),
            });
        };

        let mut scanner = MboxScanner::new(0, opts);
        let mut fresh = scanner.feed(fs.mbox());
        fresh.extend(scanner.finish());
        assert_eq!(out.index.entries, fresh);
        assert_eq!(fresh[0].header, b"Subject: one\nX-Status: A\n");
    }

    #[test]
    fn failed_copy_back_keeps_the_temp_file() {
        let entries = scan(MBOX);
        let mut fs = memfs(MBOX);
        let edits = BTreeMap::from([(entries[0].id.clone(), MboxEntryUpdateEdit::Remove)]);
        let opts = MboxEntryUpdateOptions {
            lock: MboxLockOptions {
                skip_dotlock: true,
                skip_fcntl: true,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut cor = MboxEntryUpdate::new(MboxFsPath::new("mbox"), edits, opts);
        let mut arg = None;
        let result = loop {
            let y = match cor.resume(arg.take()) {
                MboxCoroutineState::Complete(result) => break result,
                MboxCoroutineState::Yielded(y) => y,
            };
            if let MboxYield::WantsFileRead { path, .. } = &y
                && path.as_str() == "temp1"
            {
                arg = Some(MboxReply::FileRead(Vec::new()));
                continue;
            }
            arg = Some(fs.answer(y));
        };
        assert!(matches!(result, Err(MboxEntryUpdateError::Copy(_))));
        assert!(
            fs.files.contains_key("temp1"),
            "temp file kept for recovery"
        );
    }

    #[test]
    fn short_reads_remove_the_temp_file() {
        let entries = scan(MBOX);
        let mut fs = memfs(MBOX);
        let edits = BTreeMap::from([(entries[0].id.clone(), MboxEntryUpdateEdit::Remove)]);
        let opts = MboxEntryUpdateOptions {
            lock: MboxLockOptions {
                skip_dotlock: true,
                skip_fcntl: true,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut cor = MboxEntryUpdate::new(MboxFsPath::new("mbox"), edits, opts);
        let mut arg = None;
        let mut reads = 0;
        let result = loop {
            let y = match cor.resume(arg.take()) {
                MboxCoroutineState::Complete(result) => break result,
                MboxCoroutineState::Yielded(y) => y,
            };
            if let MboxYield::WantsFileRead { .. } = y {
                reads += 1;
                if reads == 4 {
                    arg = Some(MboxReply::FileRead(Vec::new()));
                    continue;
                }
            }
            arg = Some(fs.answer(y));
        };
        assert!(matches!(result, Err(MboxEntryUpdateError::Copy(_))));
        assert_eq!(fs.files.len(), 1, "temp file removed");
        assert_eq!(fs.mbox(), MBOX);
    }
}
