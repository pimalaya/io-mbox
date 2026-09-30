//! # Lock
//!
//! The two locks an mbox writer holds, taken and released in the order
//! MTAs and MUAs agree on: first the dotlock (the file `<mbox>.lock`,
//! created exclusively), then an fcntl write lock on the mbox itself.
//! A dotlock older than [`MboxLockOptions::stale_secs`] is taken as
//! left behind by a crashed writer and broken.
//!
//! [`acquire`] takes both and [`release`] drops them. The write
//! coroutines of [`crate::entry`] and [`crate::mbox`] run them around
//! their own steps, so a caller never writes unlocked by mistake.
//!
//! Refs: <https://doc.dovecot.org/admin_manual/mbox/mbox_locking/>

pub mod acquire;
pub mod release;

use crate::path::MboxFsPath;

/// Default time to wait for a lock.
pub(crate) const TIMEOUT_SECS: u64 = 10;

/// Default age after which a dotlock is broken.
pub(crate) const STALE_SECS: u64 = 300;

/// Delay between two attempts at a busy lock.
pub(crate) const RETRY_MS: u64 = 100;

/// Which locks to take, and how long to wait for them.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct MboxLockOptions {
    /// Do not take the dotlock, for a spool directory the user cannot
    /// write to. Other writers then rely on fcntl alone.
    pub skip_dotlock: bool,
    /// Do not take the fcntl lock.
    pub skip_fcntl: bool,
    /// Seconds to wait for a busy lock, 10 when unset.
    pub timeout_secs: Option<u64>,
    /// Age in seconds after which a dotlock is broken, 300 when unset.
    pub stale_secs: Option<u64>,
}

/// The locks held on an mbox file.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct MboxLock {
    /// The locked mbox.
    pub path: MboxFsPath,
    /// Whether the dotlock is held.
    pub dotlock: bool,
    /// Whether the fcntl lock is held.
    pub fcntl: bool,
}

impl MboxLock {
    /// Path of the dotlock of `path`.
    pub fn dotlock_path(path: &MboxFsPath) -> MboxFsPath {
        path.with_suffix(".lock")
    }
}
