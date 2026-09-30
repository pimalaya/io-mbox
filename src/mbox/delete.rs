//! I/O-free coroutine deleting an mbox file, under lock so no delivery
//! lands in it while it goes.

use core::fmt;

use log::debug;
use thiserror::Error;

use crate::{
    coroutine::*,
    lock::{MboxLock, MboxLockOptions, acquire::*, release::*},
    mbox_try,
    path::MboxFsPath,
};

/// Failure causes of a [`MboxDelete`] step.
#[derive(Clone, Debug, Error)]
pub enum MboxDeleteError {
    /// A reply arrived that does not match the awaited step.
    #[error("Mbox delete failed: unexpected arg {0:?}")]
    UnexpectedArg(Option<MboxReply>),
    /// The mbox does not exist.
    #[error("Mbox delete failed: {0} not found")]
    NotFound(MboxFsPath),
    /// The locks could not be taken.
    #[error(transparent)]
    Lock(#[from] MboxLockAcquireError),
    /// The locks could not be released.
    #[error(transparent)]
    Unlock(#[from] MboxLockReleaseError),
}

/// Deletes an mbox file.
#[derive(Debug)]
pub struct MboxDelete {
    path: MboxFsPath,
    opts: MboxLockOptions,
    lock: Option<MboxLock>,
    result: Option<Result<(), MboxDeleteError>>,
    state: State,
}

impl MboxDelete {
    /// Builds a coroutine deleting the mbox at `path`.
    pub fn new(path: MboxFsPath, opts: MboxLockOptions) -> Self {
        Self {
            path,
            opts,
            lock: None,
            result: None,
            state: State::Check,
        }
    }

    fn finish(
        &mut self,
        result: Result<(), MboxDeleteError>,
    ) -> MboxCoroutineState<MboxYield, <Self as MboxCoroutine>::Return> {
        let Some(lock) = self.lock.take() else {
            return MboxCoroutineState::Complete(result);
        };
        self.result = Some(result);
        self.state = State::Release(MboxLockRelease::new(lock));
        self.resume(None)
    }
}

impl MboxCoroutine for MboxDelete {
    type Yield = MboxYield;
    type Return = Result<(), MboxDeleteError>;

    fn resume(&mut self, arg: Option<MboxReply>) -> MboxCoroutineState<Self::Yield, Self::Return> {
        match (&mut self.state, arg) {
            (State::Check, None) => {
                self.state = State::Checking;
                MboxCoroutineState::Yielded(MboxYield::WantsFileMeta(self.path.clone()))
            }
            (State::Checking, Some(MboxReply::FileMeta(None))) => {
                MboxCoroutineState::Complete(Err(MboxDeleteError::NotFound(self.path.clone())))
            }
            (State::Checking, Some(MboxReply::FileMeta(Some(_)))) => {
                let acquire = MboxLockAcquire::new(self.path.clone(), self.opts.clone());
                self.state = State::Lock(acquire);
                self.resume(None)
            }
            (State::Lock(acquire), arg) => {
                self.lock = Some(mbox_try!(acquire, arg));
                self.state = State::Stat;
                MboxCoroutineState::Yielded(MboxYield::WantsFileMeta(self.path.clone()))
            }
            (State::Stat, Some(MboxReply::FileMeta(None))) => {
                self.finish(Err(MboxDeleteError::NotFound(self.path.clone())))
            }
            (State::Stat, Some(MboxReply::FileMeta(Some(_)))) => {
                self.state = State::Remove;
                MboxCoroutineState::Yielded(MboxYield::WantsFileRemove(self.path.clone()))
            }
            (State::Remove, Some(MboxReply::FileRemove)) => {
                debug!("mbox deleted");
                self.finish(Ok(()))
            }
            (State::Release(release), arg) => {
                mbox_try!(release, arg);
                self.state = State::Done;
                MboxCoroutineState::Complete(self.result.take().unwrap_or(Ok(())))
            }
            (State::Done, arg) => {
                MboxCoroutineState::Complete(Err(MboxDeleteError::UnexpectedArg(arg)))
            }
            (_, arg) => self.finish(Err(MboxDeleteError::UnexpectedArg(arg))),
        }
    }
}

#[derive(Debug)]
enum State {
    Check,
    Checking,
    Lock(MboxLockAcquire),
    Stat,
    Remove,
    Release(MboxLockRelease),
    Done,
}

impl fmt::Display for State {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Check => f.write_str("start"),
            Self::Checking => f.write_str("check mbox"),
            Self::Lock(_) => f.write_str("lock mbox"),
            Self::Stat => f.write_str("stat mbox"),
            Self::Remove => f.write_str("remove mbox"),
            Self::Release(_) => f.write_str("unlock mbox"),
            Self::Done => f.write_str("done"),
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::{coroutine::*, lock::MboxLockOptions, mbox::delete::*, path::MboxFsPath};

    fn delete() -> MboxDelete {
        let lock = MboxLockOptions {
            skip_dotlock: true,
            ..Default::default()
        };
        MboxDelete::new(MboxFsPath::new("a"), lock)
    }

    #[test]
    fn removes_under_lock() {
        let mut cor = delete();
        cor.resume(None);
        cor.resume(Some(MboxReply::FileMeta(Some(Default::default()))));
        assert!(matches!(
            cor.resume(Some(MboxReply::FcntlLock(true))),
            MboxCoroutineState::Yielded(MboxYield::WantsFileMeta(_))
        ));
        let meta = Some(MboxFileMeta::default());
        assert!(matches!(
            cor.resume(Some(MboxReply::FileMeta(meta))),
            MboxCoroutineState::Yielded(MboxYield::WantsFileRemove(_))
        ));
        assert!(matches!(
            cor.resume(Some(MboxReply::FileRemove)),
            MboxCoroutineState::Yielded(MboxYield::WantsFcntlUnlock(_))
        ));
        assert!(matches!(
            cor.resume(Some(MboxReply::FcntlUnlock)),
            MboxCoroutineState::Complete(Ok(()))
        ));
        assert!(matches!(
            cor.resume(None),
            MboxCoroutineState::Complete(Err(MboxDeleteError::UnexpectedArg(None)))
        ));
    }

    #[test]
    fn missing_and_bad_replies_unlock() {
        let mut cor = delete();
        cor.resume(None);
        assert!(matches!(
            cor.resume(Some(MboxReply::FileMeta(None))),
            MboxCoroutineState::Complete(Err(MboxDeleteError::NotFound(_)))
        ));

        let mut cor = delete();
        cor.resume(None);
        cor.resume(Some(MboxReply::FileMeta(Some(Default::default()))));
        cor.resume(Some(MboxReply::FcntlLock(true)));
        cor.resume(Some(MboxReply::FileMeta(None)));
        assert!(matches!(
            cor.resume(Some(MboxReply::FcntlUnlock)),
            MboxCoroutineState::Complete(Err(MboxDeleteError::NotFound(_)))
        ));

        let mut cor = delete();
        cor.resume(None);
        cor.resume(Some(MboxReply::FileMeta(Some(Default::default()))));
        cor.resume(Some(MboxReply::FcntlLock(true)));
        cor.resume(Some(MboxReply::Sleep));
        assert!(matches!(
            cor.resume(Some(MboxReply::FcntlUnlock)),
            MboxCoroutineState::Complete(Err(MboxDeleteError::UnexpectedArg(_)))
        ));

        let mut cor = delete();
        cor.resume(None);
        cor.resume(Some(MboxReply::FileMeta(Some(Default::default()))));
        assert!(matches!(
            cor.resume(Some(MboxReply::Sleep)),
            MboxCoroutineState::Complete(Err(MboxDeleteError::Lock(_)))
        ));
    }
}
