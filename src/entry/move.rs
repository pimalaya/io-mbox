//! I/O-free coroutine moving messages from one mbox to another.
//!
//! A move is a [`MboxEntryCopy`] followed by a [`MboxEntryUpdate`]
//! removing the copied messages from the source. The two files are
//! never locked together: a crash between the steps leaves the messages
//! in both files, never in neither.

use core::{fmt, mem};

use alloc::{boxed::Box, collections::BTreeMap, string::String, vec::Vec};

use log::debug;
use thiserror::Error;

use crate::{
    coroutine::*,
    entry::{
        MboxEntry,
        copy::{MboxEntryCopy, MboxEntryCopyError, MboxEntryCopyOptions},
        update::{
            MboxEntryUpdate, MboxEntryUpdateEdit, MboxEntryUpdateError, MboxEntryUpdateOptions,
        },
    },
    format::MboxFormat,
    index::MboxIndex,
    lock::MboxLockOptions,
    mbox_try,
    path::MboxFsPath,
    scan::MboxScannerOptions,
};

/// Failure causes of a [`MboxEntryMove`] step.
#[derive(Clone, Debug, Error)]
pub enum MboxEntryMoveError {
    /// The messages could not be copied.
    #[error(transparent)]
    Copy(#[from] MboxEntryCopyError),
    /// The messages could not be removed from the source.
    #[error(transparent)]
    Update(#[from] MboxEntryUpdateError),
}

/// Options of a [`MboxEntryMove`].
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct MboxEntryMoveOptions {
    /// Index of the source, synced under lock before the removal.
    pub index: Option<MboxIndex>,
    /// Format of both files, mboxrd when unset.
    pub format: MboxFormat,
    /// Locks to take on each file.
    pub lock: MboxLockOptions,
    /// Scanner options the entries are produced with.
    pub scanner: MboxScannerOptions,
    /// Size of a read, 64 KiB when unset.
    pub chunk_size: Option<usize>,
}

/// Result of a [`MboxEntryMove`].
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct MboxEntryMoveOutput {
    /// The moved messages, as appended to the target.
    pub entries: Vec<MboxEntry>,
    /// Index of the source once the messages are removed.
    pub index: MboxIndex,
}

/// Moves messages from one mbox to the end of another.
#[derive(Debug)]
pub struct MboxEntryMove {
    src: MboxFsPath,
    ids: Vec<String>,
    opts: MboxEntryMoveOptions,
    moved: Vec<MboxEntry>,
    state: State,
}

impl MboxEntryMove {
    /// Builds a coroutine moving `entries` of the mbox at `src` to the
    /// mbox at `dst`.
    pub fn new(
        src: MboxFsPath,
        dst: MboxFsPath,
        entries: Vec<MboxEntry>,
        opts: MboxEntryMoveOptions,
    ) -> Self {
        let ids = entries.iter().map(|entry| entry.id.clone()).collect();
        let copy_opts = MboxEntryCopyOptions {
            format: opts.format,
            lock: opts.lock.clone(),
            scanner: opts.scanner.clone(),
            chunk_size: opts.chunk_size,
        };
        let copy = MboxEntryCopy::new(src.clone(), dst, entries, copy_opts);
        Self {
            src,
            ids,
            opts,
            moved: Vec::new(),
            state: State::Copy(Box::new(copy)),
        }
    }
}

impl MboxCoroutine for MboxEntryMove {
    type Yield = MboxYield;
    type Return = Result<MboxEntryMoveOutput, MboxEntryMoveError>;

    fn resume(&mut self, arg: Option<MboxReply>) -> MboxCoroutineState<Self::Yield, Self::Return> {
        match &mut self.state {
            State::Copy(copy) => {
                self.moved = mbox_try!(copy.as_mut(), arg);
                let edits: BTreeMap<_, _> = self
                    .ids
                    .drain(..)
                    .map(|id| (id, MboxEntryUpdateEdit::Remove))
                    .collect();
                let opts = MboxEntryUpdateOptions {
                    index: self.opts.index.take(),
                    lock: self.opts.lock.clone(),
                    scanner: self.opts.scanner.clone(),
                    chunk_size: self.opts.chunk_size,
                };
                let update = MboxEntryUpdate::new(self.src.clone(), edits, opts);
                self.state = State::Remove(Box::new(update));
                self.resume(None)
            }
            State::Remove(update) => {
                let output = mbox_try!(update.as_mut(), arg);
                debug!("moved mbox messages");
                let entries = mem::take(&mut self.moved);
                MboxCoroutineState::Complete(Ok(MboxEntryMoveOutput {
                    entries,
                    index: output.index,
                }))
            }
        }
    }
}

#[derive(Debug)]
enum State {
    Copy(Box<MboxEntryCopy>),
    Remove(Box<MboxEntryUpdate>),
}

impl fmt::Display for State {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Copy(_) => f.write_str("copy messages"),
            Self::Remove(_) => f.write_str("remove messages"),
        }
    }
}
