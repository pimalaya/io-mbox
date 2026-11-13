//! # Client
//!
//! [`MboxClient`], the std-blocking driver running any coroutine of the
//! crate against the local filesystem, plus one helper per operation
//! resolving mailbox names through its [`MboxStore`].
//!
//! Files are opened once per run and kept open while the coroutine reads
//! and writes them. The fcntl lock is an open file description lock
//! (`F_OFD_SETLK`) on Linux, which conflicts with the classic POSIX
//! locks MTAs take, and a classic `F_SETLK` on other Unix systems. On
//! Windows the fcntl lock is a no-op and the dotlock does the work.
//!
//! A dotlock holds the pid of the process, and is only refreshed or
//! removed while it still is the file this run created: another writer
//! may have broken and replaced it.
//!
//! The temporary file of a rewrite is created beside the mbox, hidden and
//! readable by its owner alone, so it shares its filesystem and outlives
//! a reboot. The system directory is the fallback where the directory of
//! the mbox cannot be written to.
//!
//! A run that fails removes the dotlocks it created, and closes the files
//! it opened (which drops their fcntl locks). It removes its temporary
//! file too, unless the copy back over the mbox had started: the file is
//! then the intact copy of the mbox tail, kept and named in the error
//! with the mbox and the offset it belongs at.

use core::mem;

use alloc::{
    boxed::Box,
    collections::{BTreeMap, BTreeSet, btree_map::Entry},
    string::{String, ToString},
    vec::Vec,
};

#[cfg(unix)]
use std::os::{
    fd::AsRawFd,
    unix::fs::{MetadataExt, OpenOptionsExt},
};
use std::{
    env,
    fs::{self, File, OpenOptions},
    io::{self, Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    process,
    sync::atomic::{AtomicU32, Ordering},
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use log::{trace, warn};
use thiserror::Error;

use crate::{
    coroutine::*,
    entry::{MboxEntry, MboxFullEntry, append::*, copy::*, get::*, r#move::*, update::*},
    format::MboxFormat,
    from_line::{HEAD_LEN, MboxFromLine},
    index::{MboxIndex, sync::*},
    lock::{MboxLockOptions, acquire::MboxLockAcquireError},
    mbox::{Mbox, create::*, delete::*, list::*, rename::*},
    path::{MboxFsPath, MboxPath},
    range::MboxRangeCopyError,
    scan::MboxScannerOptions,
    store::MboxStore,
};

/// Counter making temporary file names unique within the process.
static TEMP_COUNTER: AtomicU32 = AtomicU32::new(0);

/// Errors returned by [`MboxClient`].
#[derive(Debug, Error)]
pub enum MboxClientError {
    /// The index sync coroutine failed.
    #[error(transparent)]
    IndexSync(#[from] MboxIndexSyncError),
    /// The entry get coroutine failed.
    #[error(transparent)]
    EntryGet(#[from] MboxEntryGetError),
    /// The entry append coroutine failed.
    #[error(transparent)]
    EntryAppend(#[from] MboxEntryAppendError),
    /// The entry update coroutine failed.
    #[error(transparent)]
    EntryUpdate(#[from] MboxEntryUpdateError),
    /// The entry copy coroutine failed.
    #[error(transparent)]
    EntryCopy(#[from] MboxEntryCopyError),
    /// The entry move coroutine failed.
    #[error(transparent)]
    EntryMove(#[from] MboxEntryMoveError),
    /// The range copy coroutine failed.
    #[error(transparent)]
    RangeCopy(#[from] MboxRangeCopyError),
    /// The mbox create coroutine failed.
    #[error(transparent)]
    Create(#[from] MboxCreateError),
    /// The mbox delete coroutine failed.
    #[error(transparent)]
    Delete(#[from] MboxDeleteError),
    /// The mbox list coroutine failed.
    #[error(transparent)]
    List(#[from] MboxListError),
    /// The mbox rename coroutine failed.
    #[error(transparent)]
    Rename(#[from] MboxRenameError),
    /// The name stands for no mbox below the root: it is empty, holds a
    /// NUL or a `.` or `..` segment, or names a dotlock or a summary.
    #[error(
        "Mbox name {0:?} is invalid: empty, holding a NUL or a `.` or `..` segment, or named like a `.lock` or `.msf` side file"
    )]
    InvalidName(String),
    /// No message has this id.
    #[error("Mbox message {0} not found")]
    MessageNotFound(String),
    /// The dotlock could not be created for lack of permission.
    #[error(
        "Cannot create dotlock {0}: permission denied, skip dotlocking to rely on fcntl locks alone"
    )]
    DotlockDenied(MboxFsPath),
    /// A run failed while a temporary file was being copied back over an
    /// mbox: it holds the only intact copy of the mbox from `offset` on.
    #[error("{source}, temporary file kept at {temp}: it holds {path} from offset {offset} on")]
    Interrupted {
        /// Why the run failed.
        source: Box<MboxClientError>,
        /// The temporary file to recover from.
        temp: MboxFsPath,
        /// The mbox it was being copied back over.
        path: MboxFsPath,
        /// Where in the mbox its first byte belongs.
        offset: u64,
    },
    /// A filesystem operation failed.
    #[error(transparent)]
    Io(#[from] io::Error),
}

impl MboxClientError {
    /// The lock another process held past the timeout, whichever write
    /// wanted it, so a caller can tell a busy mbox, worth a retry, from
    /// any other failure.
    pub fn lock_busy(&self) -> Option<&MboxLockAcquireError> {
        let lock = match self {
            Self::EntryAppend(MboxEntryAppendError::Lock(err))
            | Self::EntryUpdate(MboxEntryUpdateError::Lock(err))
            | Self::EntryCopy(MboxEntryCopyError::Append(MboxEntryAppendError::Lock(err)))
            | Self::EntryMove(MboxEntryMoveError::Copy(MboxEntryCopyError::Append(
                MboxEntryAppendError::Lock(err),
            )))
            | Self::EntryMove(MboxEntryMoveError::Update(MboxEntryUpdateError::Lock(err)))
            | Self::Delete(MboxDeleteError::Lock(err)) => err,
            Self::Interrupted { source, .. } => return source.lock_busy(),
            _ => return None,
        };

        match lock {
            MboxLockAcquireError::DotlockBusy(_) | MboxLockAcquireError::FcntlBusy(_) => Some(lock),
            MboxLockAcquireError::UnexpectedArg(_) => None,
        }
    }
}

/// Std-blocking mbox client.
#[derive(Clone, Debug, Default)]
pub struct MboxClient {
    /// Root, spool and layout.
    pub store: MboxStore,
    /// Format written by appends and undone by reads.
    pub format: MboxFormat,
    /// Locks taken by writes.
    pub lock: MboxLockOptions,
    /// Scanner options, `Content-Length` trusted for the mboxcl variants.
    pub scanner: MboxScannerOptions,
    /// Size of a read, 64 KiB when unset.
    pub chunk_size: Option<usize>,
    /// Directory holding the temporary files of rewrites. Unset, the one
    /// of the mbox, or the system one where that cannot be written to. A
    /// rewrite of a large mbox needs as much free room there as the
    /// rewritten tail.
    pub temp_dir: Option<PathBuf>,
}

impl MboxClient {
    /// Builds a client rooted at `root`, with every other option at its
    /// default.
    pub fn new(root: impl Into<MboxFsPath>) -> Self {
        Self {
            store: MboxStore {
                root: root.into(),
                ..Default::default()
            },
            ..Default::default()
        }
    }

    /// Runs any coroutine of the crate to completion against the local
    /// filesystem.
    pub fn run<C, T, E>(&self, mut coroutine: C) -> Result<T, MboxClientError>
    where
        C: MboxCoroutine<Yield = MboxYield, Return = Result<T, E>>,
        MboxClientError: From<E>,
    {
        let mut run = Run::default();
        let mut arg = None;

        loop {
            let y = match coroutine.resume(arg.take()) {
                MboxCoroutineState::Complete(Ok(out)) => return Ok(out),
                MboxCoroutineState::Complete(Err(err)) => return Err(run.abort(err.into())),
                MboxCoroutineState::Yielded(y) => y,
            };

            match run.answer(y, self.temp_dir.as_ref()) {
                Ok(reply) => arg = Some(reply),
                Err(err) => return Err(run.abort(err)),
            }
        }
    }

    fn path(&self, name: impl Into<MboxPath>) -> Result<MboxFsPath, MboxClientError> {
        let name = name.into();
        let path = self.store.resolve(&name);
        path.ok_or_else(|| MboxClientError::InvalidName(name.to_string()))
    }

    /// Lists the mailboxes of the store.
    ///
    /// A file below the root that does not open on a `From_` line, as
    /// [`MboxFromLine::opens_mbox`] tells, is no mailbox and is left out.
    /// One that cannot be read is kept, and the spool is unless it is a
    /// directory, which no read opens.
    pub fn list_mboxes(&self) -> Result<BTreeSet<Mbox>, MboxClientError> {
        let mut mboxes = self.run(MboxList::new(self.store.clone()))?;
        mboxes.retain(|mbox| match self.store.inbox.as_ref() == Some(&mbox.path) {
            true => !fs::metadata(mbox.path.as_str()).is_ok_and(|meta| meta.is_dir()),
            false => opens_mbox(&mbox.path),
        });
        Ok(mboxes)
    }

    /// Creates the empty mailbox `name`.
    pub fn create_mbox(&self, name: impl Into<MboxPath>) -> Result<(), MboxClientError> {
        self.run(MboxCreate::new(self.path(name)?))
    }

    /// Deletes the mailbox `name`, under lock.
    pub fn delete_mbox(&self, name: impl Into<MboxPath>) -> Result<(), MboxClientError> {
        self.run(MboxDelete::new(self.path(name)?, self.lock.clone()))
    }

    /// Renames the mailbox `from` to `to`.
    pub fn rename_mbox(
        &self,
        from: impl Into<MboxPath>,
        to: impl Into<MboxPath>,
    ) -> Result<(), MboxClientError> {
        self.run(MboxRename::new(self.path(from)?, self.path(to)?))
    }

    /// Brings `index`, if any, up to date with the mailbox `name`.
    pub fn sync_index(
        &self,
        name: impl Into<MboxPath>,
        index: Option<MboxIndex>,
    ) -> Result<MboxIndexSyncOutput, MboxClientError> {
        let opts = MboxIndexSyncOptions {
            scanner: self.scanner.clone(),
            chunk_size: self.chunk_size,
        };
        self.run(MboxIndexSync::new(self.path(name)?, index, opts))
    }

    /// Reads the message of `entry` in the mailbox `name`.
    pub fn get(
        &self,
        name: impl Into<MboxPath>,
        entry: MboxEntry,
    ) -> Result<MboxFullEntry, MboxClientError> {
        let opts = MboxEntryGetOptions {
            format: self.format,
            scanner: self.scanner.clone(),
            chunk_size: self.chunk_size,
        };
        self.run(MboxEntryGet::new(self.path(name)?, entry, opts))
    }

    /// Reads the message `id` of the mailbox `name`, syncing `index`
    /// first.
    pub fn get_by_id(
        &self,
        name: impl Into<MboxPath>,
        id: &str,
        index: Option<MboxIndex>,
    ) -> Result<MboxFullEntry, MboxClientError> {
        let name = name.into();
        let index = self.sync_index(name.clone(), index)?.index;
        let entry = index
            .get(id)
            .cloned()
            .ok_or_else(|| MboxClientError::MessageNotFound(id.to_string()))?;
        self.get(name, entry)
    }

    /// Appends messages to the mailbox `name`, under lock.
    pub fn append(
        &self,
        name: impl Into<MboxPath>,
        items: Vec<MboxEntryAppendItem>,
    ) -> Result<Vec<MboxEntry>, MboxClientError> {
        let opts = MboxEntryAppendOptions {
            format: self.format,
            lock: self.lock.clone(),
            scanner: self.scanner.clone(),
        };
        self.run(MboxEntryAppend::new(self.path(name)?, items, opts))
    }

    /// Rewrites flags and removes messages of the mailbox `name`, under
    /// lock.
    pub fn update(
        &self,
        name: impl Into<MboxPath>,
        edits: BTreeMap<String, MboxEntryUpdateEdit>,
        index: Option<MboxIndex>,
    ) -> Result<MboxEntryUpdateOutput, MboxClientError> {
        let opts = MboxEntryUpdateOptions {
            index,
            lock: self.lock.clone(),
            scanner: self.scanner.clone(),
            chunk_size: self.chunk_size,
        };
        self.run(MboxEntryUpdate::new(self.path(name)?, edits, opts))
    }

    /// Copies `entries` of the mailbox `from` to the end of `to`.
    pub fn copy(
        &self,
        from: impl Into<MboxPath>,
        to: impl Into<MboxPath>,
        entries: Vec<MboxEntry>,
    ) -> Result<Vec<MboxEntry>, MboxClientError> {
        let opts = MboxEntryCopyOptions {
            format: self.format,
            lock: self.lock.clone(),
            scanner: self.scanner.clone(),
            chunk_size: self.chunk_size,
        };
        self.run(MboxEntryCopy::new(
            self.path(from)?,
            self.path(to)?,
            entries,
            opts,
        ))
    }

    /// Moves `entries` of the mailbox `from` to the end of `to`.
    pub fn r#move(
        &self,
        from: impl Into<MboxPath>,
        to: impl Into<MboxPath>,
        entries: Vec<MboxEntry>,
        index: Option<MboxIndex>,
    ) -> Result<MboxEntryMoveOutput, MboxClientError> {
        let opts = MboxEntryMoveOptions {
            index,
            format: self.format,
            lock: self.lock.clone(),
            scanner: self.scanner.clone(),
            chunk_size: self.chunk_size,
        };
        self.run(MboxEntryMove::new(
            self.path(from)?,
            self.path(to)?,
            entries,
            opts,
        ))
    }
}

/// Filesystem state of one [`MboxClient::run`].
#[derive(Default)]
struct Run {
    /// Files open for reading.
    readers: BTreeMap<MboxFsPath, File>,
    /// Files open for writing, also holding the fcntl locks.
    writers: BTreeMap<MboxFsPath, File>,
    /// Dotlocks created and not yet removed.
    dotlocks: BTreeMap<MboxFsPath, Dotlock>,
    /// Temporary files created and not yet removed.
    temps: BTreeMap<MboxFsPath, Temp>,
}

/// What a temporary file is for.
struct Temp {
    /// The mbox it holds the tail of.
    path: MboxFsPath,
    /// Where that tail starts in the mbox.
    offset: u64,
    /// Whether the mbox was written since, the copy back having started.
    copying_back: bool,
}

impl Run {
    fn answer(
        &mut self,
        y: MboxYield,
        temp_dir: Option<&PathBuf>,
    ) -> Result<MboxReply, MboxClientError> {
        let reply = match y {
            MboxYield::WantsDirRead(path) => {
                trace!("read dir {path}");
                let mut entries = BTreeMap::new();
                match fs::read_dir(path.as_str()) {
                    Ok(iter) => {
                        for entry in iter {
                            let entry = entry?;
                            let entry_path = entry.path();
                            let link = entry.file_type().is_ok_and(|kind| kind.is_symlink());
                            let kind = match fs::metadata(&entry_path) {
                                Ok(meta) if meta.is_file() => MboxFileKind::File,
                                Ok(meta) if meta.is_dir() && link => MboxFileKind::DirLink,
                                Ok(meta) if meta.is_dir() => MboxFileKind::Dir,
                                _ => MboxFileKind::Other,
                            };
                            entries.insert(MboxFsPath::from(entry_path), kind);
                        }
                    }
                    Err(err) if err.kind() == io::ErrorKind::NotFound => {}
                    Err(err) => return Err(err.into()),
                }
                MboxReply::DirRead(entries)
            }
            MboxYield::WantsDirCanonical(path) => {
                let canonical = fs::canonicalize(path.as_str()).ok();
                trace!("canonicalize {path}: {canonical:?}");
                MboxReply::DirCanonical(canonical.map(MboxFsPath::from))
            }
            MboxYield::WantsDirCreate(path) => {
                trace!("create dir {path}");
                fs::create_dir_all(path.as_str())?;
                MboxReply::DirCreate
            }
            MboxYield::WantsFileMeta(path) => {
                let meta = match fs::metadata(path.as_str()) {
                    Ok(meta) => Some(file_meta(&meta)),
                    Err(err) if err.kind() == io::ErrorKind::NotFound => None,
                    Err(err) => return Err(err.into()),
                };
                trace!("stat {path}: {meta:?}");
                MboxReply::FileMeta(meta)
            }
            MboxYield::WantsFileRead { path, offset, len } => {
                let file = match self.writers.get_mut(&path) {
                    Some(file) => file,
                    None => match self.readers.entry(path.clone()) {
                        Entry::Occupied(entry) => entry.into_mut(),
                        Entry::Vacant(entry) => entry.insert(File::open(path.as_str())?),
                    },
                };
                file.seek(SeekFrom::Start(offset))?;
                let mut bytes = Vec::with_capacity(len);
                file.take(len as u64).read_to_end(&mut bytes)?;
                MboxReply::FileRead(bytes)
            }
            MboxYield::WantsFileWrite {
                path,
                offset,
                bytes,
            } => {
                trace!("write {} bytes at {offset} of {path}", bytes.len());
                self.mark_written(&path);
                let file = self.writer(&path)?;
                let size = file.metadata()?.len();
                file.seek(SeekFrom::Start(offset))?;
                if let Err(err) = file.write_all(&bytes) {
                    if offset >= size {
                        let _ = file.set_len(size);
                    }
                    return Err(err.into());
                }
                MboxReply::FileWrite
            }
            MboxYield::WantsFileTruncate { path, len } => {
                trace!("truncate {path} to {len}");
                self.mark_written(&path);
                self.writer(&path)?.set_len(len)?;
                MboxReply::FileTruncate
            }
            MboxYield::WantsFileSync(path) => {
                self.writer(&path)?.sync_all()?;
                MboxReply::FileSync
            }
            MboxYield::WantsFileCreate(path) => {
                trace!("create {path}");
                let created = match OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(path.as_str())
                {
                    Ok(_) => true,
                    Err(err) if err.kind() == io::ErrorKind::AlreadyExists => false,
                    Err(err) => return Err(err.into()),
                };
                MboxReply::FileCreate(created)
            }
            MboxYield::WantsFileRemove(path) => {
                trace!("remove {path}");
                self.forget(&path);
                match fs::remove_file(path.as_str()) {
                    Ok(()) => {}
                    Err(err) if err.kind() == io::ErrorKind::NotFound => {}
                    Err(err) => return Err(err.into()),
                }
                self.temps.remove(&path);
                MboxReply::FileRemove
            }
            MboxYield::WantsRename { from, to } => {
                trace!("rename {from} to {to}");
                self.forget(&from);
                self.forget(&to);
                fs::rename(from.as_str(), to.as_str())?;
                MboxReply::Rename
            }
            MboxYield::WantsTempFile { path, offset } => {
                let (temp, file) = create_temp(&path, temp_dir)?;
                trace!("create temp {temp}");
                self.writers.insert(temp.clone(), file);
                let state = Temp {
                    path,
                    offset,
                    copying_back: false,
                };
                self.temps.insert(temp.clone(), state);
                MboxReply::TempFile(temp)
            }
            MboxYield::WantsDotlockCreate(path) => {
                let created = match OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(path.as_str())
                {
                    Ok(mut file) => {
                        let pid = process::id().to_string();
                        let written = file.write_all(pid.as_bytes()).is_ok();
                        let inode = file.metadata().map(|meta| file_meta(&meta).inode);
                        let dotlock = Dotlock {
                            inode: inode.ok(),
                            content: if written { pid } else { String::new() },
                        };
                        self.dotlocks.insert(path.clone(), dotlock);
                        true
                    }
                    Err(err) if err.kind() == io::ErrorKind::AlreadyExists => false,
                    Err(err) if err.kind() == io::ErrorKind::PermissionDenied => {
                        return Err(MboxClientError::DotlockDenied(path));
                    }
                    Err(err) => return Err(err.into()),
                };
                trace!("create dotlock {path}: {created}");
                MboxReply::DotlockCreate(created)
            }
            MboxYield::WantsDotlockRefresh(path) => {
                match self.dotlocks.get(&path) {
                    Some(dotlock) if dotlock.is_at(&path) => {
                        let file = OpenOptions::new().write(true).open(path.as_str());
                        let refreshed = file.and_then(|file| file.set_modified(SystemTime::now()));
                        trace!("refresh dotlock {path}: {refreshed:?}");
                    }
                    _ => warn!("dotlock {path} was replaced by another writer"),
                }
                MboxReply::DotlockRefresh
            }
            MboxYield::WantsDotlockRemove(path) => {
                // NOTE: a lock taken in an earlier run is known by its pid.
                let dotlock = self.dotlocks.remove(&path).unwrap_or_else(|| Dotlock {
                    inode: None,
                    content: process::id().to_string(),
                });
                if dotlock.is_at(&path) {
                    trace!("remove dotlock {path}");
                    match fs::remove_file(path.as_str()) {
                        Ok(()) => {}
                        Err(err) if err.kind() == io::ErrorKind::NotFound => {}
                        Err(err) => return Err(err.into()),
                    }
                } else {
                    warn!("dotlock {path} was replaced by another writer, left in place");
                }
                MboxReply::DotlockRemove
            }
            MboxYield::WantsFcntlLock(path) => {
                let file = self.writer(&path)?;
                let locked = fcntl_lock(file, true)?;
                trace!("fcntl lock {path}: {locked}");
                MboxReply::FcntlLock(locked)
            }
            MboxYield::WantsFcntlUnlock(path) => {
                trace!("fcntl unlock {path}");
                if let Some(file) = self.writers.get(&path) {
                    fcntl_lock(file, false)?;
                }
                MboxReply::FcntlUnlock
            }
            MboxYield::WantsTime => {
                let now = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default();
                MboxReply::Time {
                    secs: now.as_secs(),
                    nanos: now.subsec_nanos(),
                }
            }
            MboxYield::WantsSleep(millis) => {
                thread::sleep(Duration::from_millis(millis));
                MboxReply::Sleep
            }
        };

        Ok(reply)
    }

    /// Returns the file open for writing at `path`, opening it first.
    fn writer(&mut self, path: &MboxFsPath) -> io::Result<&mut File> {
        if !self.writers.contains_key(path) {
            let file = OpenOptions::new()
                .read(true)
                .write(true)
                .open(path.as_str())?;
            self.readers.remove(path);
            self.writers.insert(path.clone(), file);
        }
        Ok(self.writers.get_mut(path).expect("writer just inserted"))
    }

    /// Closes the files open at `path`.
    fn forget(&mut self, path: &MboxFsPath) {
        self.readers.remove(path);
        self.writers.remove(path);
    }

    /// Notes that the file at `path` changes: a temporary file holding
    /// its tail is from then on the only intact copy of it.
    fn mark_written(&mut self, path: &MboxFsPath) {
        for temp in self.temps.values_mut() {
            temp.copying_back |= temp.path == *path;
        }
    }

    /// Cleans up after a failed run: removes the dotlocks, closes the
    /// files (dropping their fcntl locks) and removes the temporary
    /// files, but the one being copied back, the only intact copy of a
    /// rewritten tail.
    fn abort(&mut self, err: MboxClientError) -> MboxClientError {
        for (path, dotlock) in mem::take(&mut self.dotlocks) {
            if dotlock.is_at(&path) {
                let _ = fs::remove_file(path.as_str());
            }
        }
        self.readers.clear();
        self.writers.clear();

        let mut kept = None;
        for (temp, state) in mem::take(&mut self.temps) {
            if state.copying_back {
                kept = Some((temp, state));
            } else {
                let _ = fs::remove_file(temp.as_str());
            }
        }

        match kept {
            Some((temp, Temp { path, offset, .. })) => {
                warn!("temporary file kept at {temp}");
                MboxClientError::Interrupted {
                    source: Box::new(err),
                    temp,
                    path,
                    offset,
                }
            }
            None => err,
        }
    }
}

/// A dotlock as this process created it.
struct Dotlock {
    /// Inode of the file, when this run created it.
    inode: Option<u64>,
    /// What the file holds: the pid, or nothing when writing it failed.
    content: String,
}

impl Dotlock {
    /// Whether the file at `path` still is this dotlock, another writer
    /// not having broken and replaced it.
    fn is_at(&self, path: &MboxFsPath) -> bool {
        let inode = fs::metadata(path.as_str()).map(|meta| file_meta(&meta).inode);
        let same_file = inode.is_ok_and(|inode| self.inode.is_none_or(|own| own == inode));
        let content = fs::read_to_string(path.as_str());
        same_file && content.is_ok_and(|content| content == self.content)
    }
}

/// Creates the temporary file of a rewrite of the mbox at `path`: in
/// `dir` when given, else beside the mbox, else in the system directory
/// when the one of the mbox cannot be written to.
fn create_temp(path: &MboxFsPath, dir: Option<&PathBuf>) -> io::Result<(MboxFsPath, File)> {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or_default();
    let count = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    let name = format!(".io-mbox-{}-{nanos}-{count}", process::id());

    let beside = path.parent().map(|parent| PathBuf::from(parent.as_str()));
    let fallback = dir.is_none().then(env::temp_dir);
    let dir = dir.cloned().or(beside).unwrap_or_default();
    let temp = dir.join(&name);

    match (create_private(&temp), fallback) {
        (Err(err), Some(fallback)) if err.kind() == io::ErrorKind::PermissionDenied => {
            let temp = fallback.join(&name);
            create_private(&temp).map(|file| (MboxFsPath::from(temp), file))
        }
        (file, _) => file.map(|file| (MboxFsPath::from(temp), file)),
    }
}

/// Creates a file its owner alone can read, failing when it exists.
#[cfg(unix)]
fn create_private(path: &Path) -> io::Result<File> {
    OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
}

/// Creates a file, failing when it exists.
#[cfg(not(unix))]
fn create_private(path: &Path) -> io::Result<File> {
    OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(path)
}

/// Whether the file at `path` opens an mbox, `true` when it cannot be
/// read.
fn opens_mbox(path: &MboxFsPath) -> bool {
    let Ok(file) = File::open(path.as_str()) else {
        return true;
    };
    let mut head = Vec::with_capacity(HEAD_LEN as usize);
    match file.take(HEAD_LEN).read_to_end(&mut head) {
        Ok(_) => MboxFromLine::opens_mbox(&head),
        Err(_) => true,
    }
}

fn file_meta(meta: &fs::Metadata) -> MboxFileMeta {
    let (mtime_secs, mtime_nanos) = match meta.modified().map(|t| t.duration_since(UNIX_EPOCH)) {
        Ok(Ok(d)) => (d.as_secs() as i64, d.subsec_nanos()),
        _ => (0, 0),
    };

    #[cfg(unix)]
    let inode = meta.ino();
    #[cfg(not(unix))]
    let inode = 0;

    MboxFileMeta {
        size: meta.len(),
        mtime_secs,
        mtime_nanos,
        inode,
    }
}

/// Takes (`lock`) or releases a whole-file write lock without blocking,
/// returning `false` when another process holds a conflicting one.
#[cfg(unix)]
fn fcntl_lock(file: &File, lock: bool) -> io::Result<bool> {
    #[cfg(any(target_os = "linux", target_os = "android"))]
    const SETLK: libc::c_int = libc::F_OFD_SETLK;
    #[cfg(not(any(target_os = "linux", target_os = "android")))]
    const SETLK: libc::c_int = libc::F_SETLK;

    // SAFETY: flock is plain data, zeroed is a valid value.
    let mut flock: libc::flock = unsafe { mem::zeroed() };
    flock.l_type = if lock { libc::F_WRLCK } else { libc::F_UNLCK } as _;
    flock.l_whence = libc::SEEK_SET as _;

    // SAFETY: the descriptor is open for the whole call, flock is a valid
    // pointer to an initialised struct.
    let ret = unsafe { libc::fcntl(file.as_raw_fd(), SETLK, &mut flock) };
    if ret == 0 {
        return Ok(true);
    }

    let err = io::Error::last_os_error();
    match err.raw_os_error() {
        Some(libc::EAGAIN) | Some(libc::EACCES) if lock => Ok(false),
        _ => Err(err),
    }
}

#[cfg(not(unix))]
fn fcntl_lock(_file: &File, _lock: bool) -> io::Result<bool> {
    Ok(true)
}

#[cfg(test)]
mod tests {
    use std::{fs, io};

    use tempfile::tempdir;

    use crate::{client::*, path::MboxFsPath};

    const MBOX: &[u8] = b"From a@b Mon Jan  1 00:00:00 2024\n\nx\n";

    fn temp(run: &mut Run, mbox: &MboxFsPath, dir: Option<&PathBuf>) -> MboxFsPath {
        let ask = MboxYield::WantsTempFile {
            path: mbox.clone(),
            offset: 7,
        };
        match run.answer(ask, dir).unwrap() {
            MboxReply::TempFile(temp) => temp,
            reply => panic!("unexpected {reply:?}"),
        }
    }

    #[test]
    fn abort_removes_dotlocks_and_keeps_a_temp_file_being_copied_back() {
        let tmp = tempdir().unwrap();
        let mbox = MboxFsPath::from(tmp.path().join("box"));
        let dotlock = MboxFsPath::from(tmp.path().join("box.lock"));
        fs::write(mbox.as_str(), MBOX).unwrap();

        let mut run = Run::default();
        let lock = MboxYield::WantsDotlockCreate(dotlock.clone());
        assert_eq!(
            run.answer(lock, None).unwrap(),
            MboxReply::DotlockCreate(true)
        );
        let unused = temp(&mut run, &mbox, None);
        let err = run.abort(io::Error::other("disk full").into());
        assert!(matches!(err, MboxClientError::Io(_)));
        assert!(!fs::exists(dotlock.as_str()).unwrap());
        assert!(!fs::exists(unused.as_str()).unwrap());

        let mut run = Run::default();
        let lock = MboxYield::WantsDotlockCreate(dotlock.clone());
        assert_eq!(
            run.answer(lock, None).unwrap(),
            MboxReply::DotlockCreate(true)
        );
        let kept = temp(&mut run, &mbox, None);
        let write = MboxYield::WantsFileWrite {
            path: mbox.clone(),
            offset: 7,
            bytes: b"y".to_vec(),
        };
        run.answer(write, None).unwrap();
        let err = run.abort(io::Error::other("disk full").into());
        assert!(!fs::exists(dotlock.as_str()).unwrap());
        assert!(fs::exists(kept.as_str()).unwrap());
        assert!(matches!(
            &err,
            MboxClientError::Interrupted { temp, path, offset: 7, .. } if *temp == kept && *path == mbox
        ));
        let message = err.to_string();
        assert!(message.contains("disk full, temporary file kept at"));
        assert!(message.ends_with(&format!("it holds {mbox} from offset 7 on")));

        let err = Run::default().abort(io::Error::other("disk full").into());
        assert!(matches!(err, MboxClientError::Io(_)));
    }

    #[cfg(unix)]
    #[test]
    fn a_temp_file_is_private_and_beside_its_mbox() {
        let tmp = tempdir().unwrap();
        let mbox = MboxFsPath::from(tmp.path().join("box"));
        let mut run = Run::default();

        let beside = temp(&mut run, &mbox, None);
        assert_eq!(beside.parent(), mbox.parent());
        assert!(beside.file_name().unwrap().starts_with(".io-mbox-"));
        let mode = fs::metadata(beside.as_str()).unwrap().mode();
        assert_eq!(mode & 0o777, 0o600);

        let dir = tmp.path().join("elsewhere");
        fs::create_dir(&dir).unwrap();
        let elsewhere = temp(&mut run, &mbox, Some(&dir));
        assert_eq!(elsewhere.parent(), Some(MboxFsPath::from(dir.as_path())));
    }

    #[cfg(unix)]
    #[test]
    fn a_temp_file_falls_back_to_the_system_directory() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = tempdir().unwrap();
        let spool = tmp.path().join("spool");
        fs::create_dir(&spool).unwrap();
        let mbox = MboxFsPath::from(spool.join("box"));
        let read_only = fs::Permissions::from_mode(0o555);
        fs::set_permissions(&spool, read_only).unwrap();

        let mut run = Run::default();
        let temp = temp(&mut run, &mbox, None);
        fs::remove_file(temp.as_str()).unwrap();

        // NOTE: root writes anywhere, the fallback then never runs.
        if fs::write(spool.join("probe"), b"").is_err() {
            assert!(Path::new(temp.as_str()).starts_with(env::temp_dir()));
            let mode = fs::Permissions::from_mode(0o755);
            fs::set_permissions(&spool, mode).unwrap();
        }
    }

    #[test]
    fn a_dotlock_replaced_by_another_writer_is_left_alone() {
        let tmp = tempdir().unwrap();
        let dotlock = MboxFsPath::from(tmp.path().join("box.lock"));
        let pid = process::id().to_string();
        let old = UNIX_EPOCH + Duration::from_secs(1000);
        let age = |path: &MboxFsPath| {
            let file = OpenOptions::new().write(true).open(path.as_str()).unwrap();
            file.set_modified(old).unwrap();
        };
        let modified = |path: &MboxFsPath| fs::metadata(path.as_str()).unwrap().modified().unwrap();

        let mut run = Run::default();
        let lock = MboxYield::WantsDotlockCreate(dotlock.clone());
        assert_eq!(
            run.answer(lock, None).unwrap(),
            MboxReply::DotlockCreate(true)
        );
        assert_eq!(fs::read_to_string(dotlock.as_str()).unwrap(), pid);
        age(&dotlock);
        let refresh = MboxYield::WantsDotlockRefresh(dotlock.clone());
        let reply = run.answer(refresh.clone(), None).unwrap();
        assert_eq!(reply, MboxReply::DotlockRefresh);
        assert!(modified(&dotlock) > old);

        fs::remove_file(dotlock.as_str()).unwrap();
        fs::write(dotlock.as_str(), b"1").unwrap();
        age(&dotlock);
        assert_eq!(
            run.answer(refresh, None).unwrap(),
            MboxReply::DotlockRefresh
        );
        assert_eq!(modified(&dotlock), old);
        let unlock = MboxYield::WantsDotlockRemove(dotlock.clone());
        let reply = run.answer(unlock.clone(), None).unwrap();
        assert_eq!(reply, MboxReply::DotlockRemove);
        assert_eq!(fs::read(dotlock.as_str()).unwrap(), b"1");

        fs::remove_file(dotlock.as_str()).unwrap();
        assert_eq!(run.answer(unlock, None).unwrap(), MboxReply::DotlockRemove);

        let mut run = Run::default();
        let lock = MboxYield::WantsDotlockCreate(dotlock.clone());
        assert_eq!(
            run.answer(lock, None).unwrap(),
            MboxReply::DotlockCreate(true)
        );
        fs::remove_file(dotlock.as_str()).unwrap();
        fs::write(dotlock.as_str(), b"1").unwrap();
        run.abort(io::Error::other("disk full").into());
        assert_eq!(fs::read(dotlock.as_str()).unwrap(), b"1");
    }

    #[test]
    fn a_busy_lock_is_found_under_any_write() {
        let busy = MboxLockAcquireError::FcntlBusy(MboxFsPath::new("a"));
        let append = MboxEntryAppendError::Lock(busy.clone());
        let errs = [
            MboxClientError::EntryAppend(append.clone()),
            MboxClientError::EntryMove(MboxEntryMoveError::Copy(MboxEntryCopyError::Append(
                append,
            ))),
            MboxClientError::Interrupted {
                source: Box::new(MboxClientError::Delete(MboxDeleteError::Lock(busy))),
                temp: MboxFsPath::new("t"),
                path: MboxFsPath::new("a"),
                offset: 0,
            },
        ];
        for err in errs {
            assert!(err.lock_busy().is_some(), "{err}");
        }

        let odd = MboxLockAcquireError::UnexpectedArg(None);
        let err = MboxClientError::EntryAppend(MboxEntryAppendError::Lock(odd));
        assert!(err.lock_busy().is_none());
        assert!(
            MboxClientError::MessageNotFound("x".into())
                .lock_busy()
                .is_none()
        );
    }

    #[test]
    fn a_listing_leaves_out_files_that_are_no_mbox() {
        let tmp = tempdir().unwrap();
        let root = tmp.path();
        fs::write(
            root.join("mbox"),
            b"From a@b Mon Jan  1 00:00:00 2024\n\nx\n",
        )
        .unwrap();
        fs::write(root.join("empty"), b"").unwrap();
        fs::write(root.join("README"), b"read me\n").unwrap();
        fs::write(root.join("spool"), b"not yet").unwrap();

        let mut client = MboxClient::new(root);
        client.store.inbox = Some(MboxFsPath::from(root.join("spool")));
        let names: Vec<String> = client
            .list_mboxes()
            .unwrap()
            .into_iter()
            .map(|mbox| mbox.name.to_string())
            .collect();
        assert_eq!(names, ["INBOX", "empty", "mbox"]);
    }

    #[test]
    fn a_listing_leaves_out_a_spool_that_is_a_directory() {
        let tmp = tempdir().unwrap();
        let root = tmp.path().join("root");
        fs::create_dir(&root).unwrap();
        fs::create_dir(tmp.path().join("spool")).unwrap();

        let mut client = MboxClient::new(root.as_path());
        client.store.inbox = Some(MboxFsPath::from(tmp.path().join("spool")));
        assert!(client.list_mboxes().unwrap().is_empty());
    }
}
