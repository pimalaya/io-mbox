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
//! A run that fails removes the dotlocks it created, and closes the files
//! it opened (which drops their fcntl locks). A temporary file it could
//! not remove is kept and named in the error: when a rewrite was being
//! copied back, it is the intact copy of the mbox tail.

use core::mem;

use alloc::{
    boxed::Box,
    collections::{BTreeMap, BTreeSet, btree_map::Entry},
    string::{String, ToString},
    vec::Vec,
};

#[cfg(unix)]
use std::os::{fd::AsRawFd, unix::fs::MetadataExt};
use std::{
    env,
    fs::{self, File, OpenOptions},
    io::{self, Read, Seek, SeekFrom, Write},
    path::PathBuf,
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
    index::{MboxIndex, sync::*},
    lock::MboxLockOptions,
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
    /// No message has this id.
    #[error("Mbox message {0} not found")]
    MessageNotFound(String),
    /// The dotlock could not be created for lack of permission.
    #[error(
        "Cannot create dotlock {0}: permission denied, skip dotlocking to rely on fcntl locks alone"
    )]
    DotlockDenied(MboxFsPath),
    /// A run failed while a temporary file it created still existed. When
    /// a rewrite was being copied back, it holds the only intact copy of
    /// the mbox tail.
    #[error("{source}, temporary file kept at {temp}")]
    Interrupted {
        /// Why the run failed.
        source: Box<MboxClientError>,
        /// The temporary file to recover from.
        temp: MboxFsPath,
    },
    /// A filesystem operation failed.
    #[error(transparent)]
    Io(#[from] io::Error),
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
    /// Directory holding the temporary files of rewrites, the system one
    /// when unset. A rewrite of a large mbox needs as much free room
    /// there as the rewritten tail.
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

    fn path(&self, name: impl Into<MboxPath>) -> MboxFsPath {
        self.store.resolve(&name.into())
    }

    /// Lists the mailboxes of the store.
    pub fn list_mboxes(&self) -> Result<BTreeSet<Mbox>, MboxClientError> {
        self.run(MboxList::new(self.store.clone()))
    }

    /// Creates the empty mailbox `name`.
    pub fn create_mbox(&self, name: impl Into<MboxPath>) -> Result<(), MboxClientError> {
        self.run(MboxCreate::new(self.path(name)))
    }

    /// Deletes the mailbox `name`, under lock.
    pub fn delete_mbox(&self, name: impl Into<MboxPath>) -> Result<(), MboxClientError> {
        self.run(MboxDelete::new(self.path(name), self.lock.clone()))
    }

    /// Renames the mailbox `from` to `to`.
    pub fn rename_mbox(
        &self,
        from: impl Into<MboxPath>,
        to: impl Into<MboxPath>,
    ) -> Result<(), MboxClientError> {
        self.run(MboxRename::new(self.path(from), self.path(to)))
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
        self.run(MboxIndexSync::new(self.path(name), index, opts))
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
        self.run(MboxEntryGet::new(self.path(name), entry, opts))
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
        self.run(MboxEntryAppend::new(self.path(name), items, opts))
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
        self.run(MboxEntryUpdate::new(self.path(name), edits, opts))
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
            self.path(from),
            self.path(to),
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
            self.path(from),
            self.path(to),
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
    dotlocks: BTreeSet<MboxFsPath>,
    /// Temporary files created and not yet removed.
    temps: BTreeSet<MboxFsPath>,
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
                            let entry_path = entry?.path();
                            let kind = match fs::metadata(&entry_path) {
                                Ok(meta) if meta.is_file() => MboxFileKind::File,
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
            MboxYield::WantsTempFile => {
                let dir = temp_dir.cloned().unwrap_or_else(env::temp_dir);
                let nanos = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map(|d| d.subsec_nanos())
                    .unwrap_or_default();
                let count = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
                let name = format!("io-mbox-{}-{nanos}-{count}", process::id());
                let path = MboxFsPath::from(dir.join(name));
                trace!("create temp {path}");
                let file = OpenOptions::new()
                    .read(true)
                    .write(true)
                    .create_new(true)
                    .open(path.as_str())?;
                self.writers.insert(path.clone(), file);
                self.temps.insert(path.clone());
                MboxReply::TempFile(path)
            }
            MboxYield::WantsDotlockCreate(path) => {
                let created = match OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(path.as_str())
                {
                    Ok(mut file) => {
                        let _ = write!(file, "{}", process::id());
                        true
                    }
                    Err(err) if err.kind() == io::ErrorKind::AlreadyExists => false,
                    Err(err) if err.kind() == io::ErrorKind::PermissionDenied => {
                        return Err(MboxClientError::DotlockDenied(path));
                    }
                    Err(err) => return Err(err.into()),
                };
                trace!("create dotlock {path}: {created}");
                if created {
                    self.dotlocks.insert(path);
                }
                MboxReply::DotlockCreate(created)
            }
            MboxYield::WantsDotlockRemove(path) => {
                trace!("remove dotlock {path}");
                fs::remove_file(path.as_str())?;
                self.dotlocks.remove(&path);
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

    /// Cleans up after a failed run: removes the dotlocks, closes the
    /// files (dropping their fcntl locks) and keeps the temporary files,
    /// which may hold the only intact copy of a rewritten tail.
    fn abort(&mut self, err: MboxClientError) -> MboxClientError {
        for path in mem::take(&mut self.dotlocks) {
            let _ = fs::remove_file(path.as_str());
        }
        self.readers.clear();
        self.writers.clear();

        match mem::take(&mut self.temps).into_iter().next() {
            Some(temp) => {
                warn!("temporary file kept at {temp}");
                MboxClientError::Interrupted {
                    source: Box::new(err),
                    temp,
                }
            }
            None => err,
        }
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

    #[test]
    fn abort_removes_dotlocks_and_keeps_temp_files() {
        let tmp = tempdir().unwrap();
        let dotlock = MboxFsPath::from(tmp.path().join("box.lock"));
        let temp = MboxFsPath::from(tmp.path().join("io-mbox-temp"));
        fs::write(dotlock.as_str(), b"").unwrap();
        fs::write(temp.as_str(), b"tail").unwrap();

        let mut run = Run::default();
        run.dotlocks.insert(dotlock.clone());
        run.temps.insert(temp.clone());

        let err = run.abort(io::Error::other("disk full").into());
        assert!(!fs::exists(dotlock.as_str()).unwrap());
        assert!(fs::exists(temp.as_str()).unwrap());
        assert!(matches!(&err, MboxClientError::Interrupted { temp: t, .. } if *t == temp));
        assert!(
            err.to_string()
                .contains("disk full, temporary file kept at")
        );

        let err = Run::default().abort(io::Error::other("disk full").into());
        assert!(matches!(err, MboxClientError::Io(_)));
    }
}
