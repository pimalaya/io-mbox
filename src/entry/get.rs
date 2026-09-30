//! I/O-free coroutine reading one message of an mbox.
//!
//! The entry comes from an index that may be stale, so the bytes read
//! are scanned again and must yield the same content id. A mismatch means
//! another client rewrote the file since, and the caller should sync
//! the index and retry.
//!
//! # Example
//!
//! ```rust,no_run
//! use io_mbox::client::MboxClient;
//!
//! let client = MboxClient::new("/path/to/mail");
//! let index = client.sync_index("archive", None).unwrap().index;
//! let entry = index.messages().next().unwrap();
//! let message = client.get("archive", entry.clone()).unwrap();
//!
//! println!("{} bytes", message.contents.len());
//! ```

use core::mem;

use alloc::{string::String, vec::Vec};

use log::{debug, trace};
use thiserror::Error;

use crate::{
    coroutine::*,
    entry::{MboxEntry, MboxFullEntry},
    format::MboxFormat,
    index::sync::CHUNK_SIZE,
    path::MboxFsPath,
    scan::{MboxScanner, MboxScannerOptions},
};

/// Failure causes of a [`MboxEntryGet`] step.
#[derive(Clone, Debug, Error)]
pub enum MboxEntryGetError {
    /// A reply arrived that does not match the awaited step.
    #[error("Mbox message get failed: unexpected arg {0:?}")]
    UnexpectedArg(Option<MboxReply>),
    /// The file no longer holds the message where the entry says.
    #[error("Mbox message get failed: message {0} moved, sync the index and retry")]
    Stale(String),
}

/// Options of a [`MboxEntryGet`].
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct MboxEntryGetOptions {
    /// Quoting to undo, mboxrd when unset.
    pub format: MboxFormat,
    /// Scanner options the entry was produced with.
    pub scanner: MboxScannerOptions,
    /// Size of a read, 64 KiB when unset.
    pub chunk_size: Option<usize>,
}

/// Reads the message of an entry, checks it still is the same message,
/// and unquotes it.
#[derive(Debug)]
pub struct MboxEntryGet {
    path: MboxFsPath,
    entry: MboxEntry,
    opts: MboxEntryGetOptions,
    bytes: Vec<u8>,
}

impl MboxEntryGet {
    /// Builds a coroutine reading `entry` from the mbox at `path`.
    pub fn new(path: MboxFsPath, entry: MboxEntry, opts: MboxEntryGetOptions) -> Self {
        let bytes = Vec::with_capacity((entry.end() - entry.offset) as usize);
        Self {
            path,
            entry,
            opts,
            bytes,
        }
    }

    fn read(&self) -> MboxCoroutineState<MboxYield, <Self as MboxCoroutine>::Return> {
        let offset = self.entry.offset + self.bytes.len() as u64;
        let remaining = self.entry.end() - offset;
        let len = (self.opts.chunk_size.unwrap_or(CHUNK_SIZE).max(1) as u64).min(remaining);
        MboxCoroutineState::Yielded(MboxYield::WantsFileRead {
            path: self.path.clone(),
            offset,
            len: len as usize,
        })
    }

    fn check(&mut self) -> MboxCoroutineState<MboxYield, <Self as MboxCoroutine>::Return> {
        let stale = MboxEntryGetError::Stale(self.entry.id.clone());

        let mut scanner = MboxScanner::new(self.entry.offset, self.opts.scanner.clone());
        let mut entries = scanner.feed(&self.bytes);
        // NOTE: the range stops before the separator, so a blank line
        // ending it is content: a synthetic separator keeps it so.
        if self.bytes.ends_with(b"\n") {
            entries.extend(scanner.feed(b"\n"));
        }
        entries.extend(scanner.finish());

        let hash = self.entry.id.split('-').next().unwrap_or_default();
        let fresh = match entries.as_slice() {
            [fresh] if fresh.id == hash && fresh.len == self.entry.len => fresh,
            _ => return MboxCoroutineState::Complete(Err(stale)),
        };

        let skip = (fresh.message_offset - fresh.offset) as usize;
        let contents = self.opts.format.unescape(&self.bytes[skip..]);

        let mut entry = mem::take(&mut self.entry);
        entry.flags = fresh.flags.clone();
        debug!("got mbox message");
        trace!("id: {}", entry.id);

        MboxCoroutineState::Complete(Ok(MboxFullEntry { entry, contents }))
    }
}

impl MboxCoroutine for MboxEntryGet {
    type Yield = MboxYield;
    type Return = Result<MboxFullEntry, MboxEntryGetError>;

    fn resume(&mut self, arg: Option<MboxReply>) -> MboxCoroutineState<Self::Yield, Self::Return> {
        let total = self.entry.end() - self.entry.offset;

        match arg {
            None if self.bytes.is_empty() && total > 0 => self.read(),
            None if total == 0 => self.check(),
            Some(MboxReply::FileRead(bytes)) => {
                if bytes.is_empty() {
                    let err = MboxEntryGetError::Stale(self.entry.id.clone());
                    return MboxCoroutineState::Complete(Err(err));
                }
                self.bytes.extend(bytes);
                self.bytes.truncate(total as usize);
                if (self.bytes.len() as u64) < total {
                    self.read()
                } else {
                    self.check()
                }
            }
            arg => MboxCoroutineState::Complete(Err(MboxEntryGetError::UnexpectedArg(arg))),
        }
    }
}

#[cfg(test)]
mod tests {
    use alloc::{string::String, vec::Vec};

    use crate::{coroutine::*, entry::get::*, flag::MboxFlag, path::MboxFsPath, scan::MboxScanner};

    const MBOX: &[u8] = b"From a@b Mon Jan  1 00:00:00 2024\nSubject: one\n\n>From here\n\nFrom c@d Mon Jan  1 00:00:00 2024\nSubject: two\nStatus: R\n\nbody\n";

    fn entries(bytes: &[u8]) -> Vec<MboxEntry> {
        let mut scanner = MboxScanner::new(0, Default::default());
        let mut entries = scanner.feed(bytes);
        entries.extend(scanner.finish());
        entries
    }

    fn run(
        file: &[u8],
        entry: MboxEntry,
        chunk: usize,
    ) -> Result<MboxFullEntry, MboxEntryGetError> {
        let opts = MboxEntryGetOptions {
            chunk_size: Some(chunk),
            ..Default::default()
        };
        let mut cor = MboxEntryGet::new(MboxFsPath::new("mbox"), entry, opts);
        let mut arg = None;
        loop {
            match cor.resume(arg.take()) {
                MboxCoroutineState::Complete(result) => return result,
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
    fn reads_and_unquotes() {
        let entries = entries(MBOX);
        for chunk in [1, 5, 4096] {
            let one = run(MBOX, entries[0].clone(), chunk).unwrap();
            assert_eq!(one.contents, b"Subject: one\n\nFrom here\n");
            let two = run(MBOX, entries[1].clone(), chunk).unwrap();
            assert_eq!(two.contents, b"Subject: two\nStatus: R\n\nbody\n");
            assert!(two.entry.flags.contains(&MboxFlag::Seen));
        }
    }

    #[test]
    fn trailing_blank_lines_are_content() {
        let bytes = b"From a@b Mon Jan  1 00:00:00 2024\nSubject: x\n\nbody\n\n\nFrom c@d Mon Jan  1 00:00:00 2024\n\n";
        let entries = entries(bytes);
        let one = run(bytes, entries[0].clone(), 4096).unwrap();
        assert_eq!(one.contents, b"Subject: x\n\nbody\n\n");
    }

    #[test]
    fn flag_changes_are_not_staleness() {
        let entries = entries(MBOX);
        let rewritten = String::from_utf8_lossy(MBOX).replace("Status: R\n", "Status: O\n");
        let two = run(rewritten.as_bytes(), entries[1].clone(), 4096).unwrap();
        assert!(two.entry.flags.contains(&MboxFlag::Old));
        assert!(!two.entry.flags.contains(&MboxFlag::Seen));
    }

    #[test]
    fn moved_messages_are_stale() {
        let entries = entries(MBOX);
        let err = run(&MBOX[1..], entries[0].clone(), 4096).unwrap_err();
        assert!(matches!(err, MboxEntryGetError::Stale(_)));
        let err = run(&MBOX[..40], entries[1].clone(), 4096).unwrap_err();
        assert!(matches!(err, MboxEntryGetError::Stale(_)));
    }

    #[test]
    fn empty_entries_and_bad_replies() {
        let mut cor =
            MboxEntryGet::new(MboxFsPath::new("x"), Default::default(), Default::default());
        assert!(matches!(
            cor.resume(None),
            MboxCoroutineState::Complete(Err(MboxEntryGetError::Stale(_)))
        ));
        let entry = entries(MBOX).remove(0);
        let mut cor = MboxEntryGet::new(MboxFsPath::new("x"), entry, Default::default());
        assert!(matches!(
            cor.resume(Some(MboxReply::Sleep)),
            MboxCoroutineState::Complete(Err(MboxEntryGetError::UnexpectedArg(_)))
        ));
    }
}
