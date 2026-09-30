//! I/O-free coroutine copying messages from one mbox to another.
//!
//! Each message is read with [`MboxEntryGet`] then appended with
//! [`MboxEntryAppend`], one at a time so memory holds a single message.
//! The copy keeps the flags and the `From_` line sender and date.

use core::{fmt, mem};

use alloc::{collections::VecDeque, vec::Vec};

use log::debug;
use thiserror::Error;

use crate::{
    coroutine::*,
    entry::{
        MboxEntry,
        append::{
            MboxEntryAppend, MboxEntryAppendError, MboxEntryAppendItem, MboxEntryAppendOptions,
        },
        get::{MboxEntryGet, MboxEntryGetError, MboxEntryGetOptions},
    },
    format::MboxFormat,
    lock::MboxLockOptions,
    mbox_try,
    path::MboxFsPath,
    scan::MboxScannerOptions,
};

/// Failure causes of a [`MboxEntryCopy`] step.
#[derive(Clone, Debug, Error)]
pub enum MboxEntryCopyError {
    /// A message could not be read.
    #[error(transparent)]
    Get(#[from] MboxEntryGetError),
    /// A message could not be appended.
    #[error(transparent)]
    Append(#[from] MboxEntryAppendError),
}

/// Options of a [`MboxEntryCopy`].
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct MboxEntryCopyOptions {
    /// Format of both files, mboxrd when unset.
    pub format: MboxFormat,
    /// Locks to take on the target.
    pub lock: MboxLockOptions,
    /// Scanner options the entries are produced with.
    pub scanner: MboxScannerOptions,
    /// Size of a read, 64 KiB when unset.
    pub chunk_size: Option<usize>,
}

/// Copies messages from one mbox to the end of another.
#[derive(Debug)]
pub struct MboxEntryCopy {
    src: MboxFsPath,
    dst: MboxFsPath,
    entries: VecDeque<MboxEntry>,
    opts: MboxEntryCopyOptions,
    copied: Vec<MboxEntry>,
    state: State,
}

impl MboxEntryCopy {
    /// Builds a coroutine copying `entries` of the mbox at `src` to the
    /// mbox at `dst`.
    pub fn new(
        src: MboxFsPath,
        dst: MboxFsPath,
        entries: Vec<MboxEntry>,
        opts: MboxEntryCopyOptions,
    ) -> Self {
        Self {
            src,
            dst,
            entries: entries.into(),
            opts,
            copied: Vec::new(),
            state: State::Next,
        }
    }

    fn next(&mut self) -> MboxCoroutineState<MboxYield, <Self as MboxCoroutine>::Return> {
        let Some(entry) = self.entries.pop_front() else {
            debug!("copied mbox messages");
            self.state = State::Next;
            return MboxCoroutineState::Complete(Ok(mem::take(&mut self.copied)));
        };

        let opts = MboxEntryGetOptions {
            format: self.opts.format,
            scanner: self.opts.scanner.clone(),
            chunk_size: self.opts.chunk_size,
        };
        self.state = State::Get(MboxEntryGet::new(self.src.clone(), entry, opts));
        self.resume(None)
    }
}

impl MboxCoroutine for MboxEntryCopy {
    type Yield = MboxYield;
    type Return = Result<Vec<MboxEntry>, MboxEntryCopyError>;

    fn resume(&mut self, arg: Option<MboxReply>) -> MboxCoroutineState<Self::Yield, Self::Return> {
        match &mut self.state {
            State::Next => self.next(),
            State::Get(get) => {
                let message = mbox_try!(get, arg);
                let item = MboxEntryAppendItem {
                    contents: message.contents,
                    flags: message.entry.flags,
                    sender: Some(message.entry.sender),
                    timestamp: Some(message.entry.timestamp),
                };
                let opts = MboxEntryAppendOptions {
                    format: self.opts.format,
                    lock: self.opts.lock.clone(),
                    scanner: self.opts.scanner.clone(),
                };
                self.state =
                    State::Append(MboxEntryAppend::new(self.dst.clone(), vec![item], opts));
                self.resume(None)
            }
            State::Append(append) => {
                let entries = mbox_try!(append, arg);
                self.copied.extend(entries);
                self.next()
            }
        }
    }
}

#[derive(Debug)]
enum State {
    Next,
    Get(MboxEntryGet),
    Append(MboxEntryAppend),
}

impl fmt::Display for State {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Next => f.write_str("pick next message"),
            Self::Get(_) => f.write_str("read message"),
            Self::Append(_) => f.write_str("append message"),
        }
    }
}
