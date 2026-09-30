//! # Coroutine
//!
//! Generator-shape coroutine contract shared by every coroutine of the
//! crate. Mirrors `core::ops::Coroutine`: a `Yield` for intermediate
//! requests, a `Return` for the terminal result, and a two-variant
//! [`MboxCoroutineState`].
//!
//! Every coroutine picks [`MboxYield`] as its `Yield`: positioned file
//! reads and writes (an mbox is one big file, streamed in chunks rather
//! than loaded), file and directory lifecycle requests, the two locks
//! (dotlock and fcntl) an mbox writer must hold, and the clock. The driver
//! answers each yield with the matching [`MboxReply`] on the next resume.
//! [`crate::client::MboxClient::run`] is the std driver.

use alloc::{collections::BTreeMap, vec::Vec};

use crate::path::MboxFsPath;

/// State yielded by a [`MboxCoroutine::resume`] step.
#[derive(Debug)]
pub enum MboxCoroutineState<Y, R> {
    /// Intermediate yield: the driver performs the request and resumes.
    Yielded(Y),
    /// Terminal yield, by convention `Result<Output, Error>`.
    Complete(R),
}

/// Standard-shape mbox coroutine.
///
/// Implementors own their state machine and declare their per-step
/// `Yield` plus a terminal `Return`. The driver reacts to each yield and
/// resumes until `Complete`.
pub trait MboxCoroutine {
    /// Intermediate value handed back on every step.
    type Yield;
    /// Terminal value, by convention `Result<Output, Error>`.
    type Return;

    /// Advances the coroutine one step.
    ///
    /// Pass [`None`] on the initial call, and `Some(reply)` carrying the
    /// answer to the previous yield afterwards.
    fn resume(&mut self, arg: Option<MboxReply>) -> MboxCoroutineState<Self::Yield, Self::Return>;
}

/// Request yielded by every io-mbox coroutine.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MboxYield {
    /// List the direct entries of a directory, answered by
    /// [`MboxReply::DirRead`]. A missing directory lists empty.
    WantsDirRead(MboxFsPath),
    /// Create a directory and its parents, answered by
    /// [`MboxReply::DirCreate`].
    WantsDirCreate(MboxFsPath),
    /// Stat a file, answered by [`MboxReply::FileMeta`].
    WantsFileMeta(MboxFsPath),
    /// Read up to `len` bytes at `offset`, answered by
    /// [`MboxReply::FileRead`]. Fewer bytes mean end of file.
    WantsFileRead {
        /// File to read.
        path: MboxFsPath,
        /// Absolute byte offset to read from.
        offset: u64,
        /// Maximum number of bytes to read.
        len: usize,
    },
    /// Write `bytes` at `offset`, extending the file when needed, answered
    /// by [`MboxReply::FileWrite`].
    WantsFileWrite {
        /// File to write, which must exist.
        path: MboxFsPath,
        /// Absolute byte offset to write at.
        offset: u64,
        /// Bytes to write.
        bytes: Vec<u8>,
    },
    /// Truncate a file to `len` bytes, answered by
    /// [`MboxReply::FileTruncate`].
    WantsFileTruncate {
        /// File to truncate.
        path: MboxFsPath,
        /// New length in bytes.
        len: u64,
    },
    /// Flush a file to stable storage, answered by
    /// [`MboxReply::FileSync`].
    WantsFileSync(MboxFsPath),
    /// Create an empty file, failing softly when it already exists,
    /// answered by [`MboxReply::FileCreate`].
    WantsFileCreate(MboxFsPath),
    /// Remove a file, answered by [`MboxReply::FileRemove`].
    WantsFileRemove(MboxFsPath),
    /// Rename a file, answered by [`MboxReply::Rename`].
    WantsRename {
        /// Current path.
        from: MboxFsPath,
        /// New path.
        to: MboxFsPath,
    },
    /// Create a fresh private temporary file, answered by
    /// [`MboxReply::TempFile`].
    WantsTempFile,
    /// Create the dotlock file exclusively (`O_CREAT | O_EXCL`), answered
    /// by [`MboxReply::DotlockCreate`].
    WantsDotlockCreate(MboxFsPath),
    /// Remove a dotlock this coroutine created, answered by
    /// [`MboxReply::DotlockRemove`].
    WantsDotlockRemove(MboxFsPath),
    /// Try to take an exclusive fcntl write lock on the whole file
    /// without blocking, answered by [`MboxReply::FcntlLock`].
    WantsFcntlLock(MboxFsPath),
    /// Release the fcntl lock taken on a file, answered by
    /// [`MboxReply::FcntlUnlock`].
    WantsFcntlUnlock(MboxFsPath),
    /// Sample the clock, answered by [`MboxReply::Time`].
    WantsTime,
    /// Sleep for the given number of milliseconds before a retry,
    /// answered by [`MboxReply::Sleep`].
    WantsSleep(u64),
}

/// Answer fed back into [`MboxCoroutine::resume`] by the driver.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MboxReply {
    /// Answer to [`MboxYield::WantsDirRead`]: every entry with its kind.
    DirRead(BTreeMap<MboxFsPath, MboxFileKind>),
    /// Acknowledgement of [`MboxYield::WantsDirCreate`].
    DirCreate,
    /// Answer to [`MboxYield::WantsFileMeta`], `None` for a missing file.
    FileMeta(Option<MboxFileMeta>),
    /// Answer to [`MboxYield::WantsFileRead`].
    FileRead(Vec<u8>),
    /// Acknowledgement of [`MboxYield::WantsFileWrite`].
    FileWrite,
    /// Acknowledgement of [`MboxYield::WantsFileTruncate`].
    FileTruncate,
    /// Acknowledgement of [`MboxYield::WantsFileSync`].
    FileSync,
    /// Answer to [`MboxYield::WantsFileCreate`]: `false` when the file
    /// already existed.
    FileCreate(bool),
    /// Acknowledgement of [`MboxYield::WantsFileRemove`].
    FileRemove,
    /// Acknowledgement of [`MboxYield::WantsRename`].
    Rename,
    /// Answer to [`MboxYield::WantsTempFile`].
    TempFile(MboxFsPath),
    /// Answer to [`MboxYield::WantsDotlockCreate`]: `false` when another
    /// process holds the dotlock.
    DotlockCreate(bool),
    /// Acknowledgement of [`MboxYield::WantsDotlockRemove`].
    DotlockRemove,
    /// Answer to [`MboxYield::WantsFcntlLock`]: `false` when another
    /// process holds a conflicting lock.
    FcntlLock(bool),
    /// Acknowledgement of [`MboxYield::WantsFcntlUnlock`].
    FcntlUnlock,
    /// Answer to [`MboxYield::WantsTime`].
    Time {
        /// Whole seconds elapsed since the Unix epoch.
        secs: u64,
        /// Sub-second remainder in nanoseconds.
        nanos: u32,
    },
    /// Acknowledgement of [`MboxYield::WantsSleep`].
    Sleep,
}

/// Kind of a directory entry.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum MboxFileKind {
    /// A regular file.
    File,
    /// A directory.
    Dir,
    /// Anything else (symlink target missing, socket, device).
    Other,
}

/// What a stat tells about an mbox file.
///
/// Stored in [`crate::index::MboxIndex`] to tell an unchanged file from a
/// rewritten one.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct MboxFileMeta {
    /// Size in bytes.
    pub size: u64,
    /// Modification time, whole seconds since the Unix epoch.
    pub mtime_secs: i64,
    /// Modification time, sub-second remainder in nanoseconds.
    pub mtime_nanos: u32,
    /// Inode number, 0 where the platform has none.
    pub inode: u64,
}

/// Coroutine `?`: forwards `Yielded`, short-circuits on `Err` (converted
/// with `Into`), evaluates to the inner `Ok` value.
#[macro_export]
macro_rules! mbox_try {
    ($coroutine:expr, $arg:expr $(,)?) => {
        match $crate::coroutine::MboxCoroutine::resume($coroutine, $arg) {
            $crate::coroutine::MboxCoroutineState::Yielded(y) => {
                return $crate::coroutine::MboxCoroutineState::Yielded(y);
            }
            $crate::coroutine::MboxCoroutineState::Complete(Err(err)) => {
                return $crate::coroutine::MboxCoroutineState::Complete(Err(err.into()));
            }
            $crate::coroutine::MboxCoroutineState::Complete(Ok(value)) => value,
        }
    };
}
