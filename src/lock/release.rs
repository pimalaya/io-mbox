//! I/O-free coroutine releasing the locks of an mbox.

use core::fmt;

use log::debug;
use thiserror::Error;

use crate::{coroutine::*, lock::MboxLock};

/// Failure causes of a [`MboxLockRelease`] step.
#[derive(Clone, Debug, Error)]
pub enum MboxLockReleaseError {
    /// A reply arrived that does not match the awaited step.
    #[error("Mbox unlock failed: unexpected arg {0:?}")]
    UnexpectedArg(Option<MboxReply>),
}

/// Releases the locks [`crate::lock::acquire::MboxLockAcquire`] took, in
/// reverse order.
#[derive(Debug)]
pub struct MboxLockRelease {
    lock: MboxLock,
    state: State,
}

impl MboxLockRelease {
    /// Builds a coroutine releasing `lock`.
    pub fn new(lock: MboxLock) -> Self {
        Self {
            lock,
            state: State::Start,
        }
    }
}

impl MboxCoroutine for MboxLockRelease {
    type Yield = MboxYield;
    type Return = Result<(), MboxLockReleaseError>;

    fn resume(&mut self, arg: Option<MboxReply>) -> MboxCoroutineState<Self::Yield, Self::Return> {
        let next = match (&self.state, arg) {
            (State::Start, None) => State::UnlockFcntl,
            (State::UnlockFcntl, Some(MboxReply::FcntlUnlock)) => State::RemoveDotlock,
            (State::RemoveDotlock, Some(MboxReply::DotlockRemove)) => State::Done,
            (_, arg) => {
                return MboxCoroutineState::Complete(Err(MboxLockReleaseError::UnexpectedArg(arg)));
            }
        };

        self.state = next;

        if self.state == State::UnlockFcntl {
            if self.lock.fcntl {
                let path = self.lock.path.clone();
                return MboxCoroutineState::Yielded(MboxYield::WantsFcntlUnlock(path));
            }
            self.state = State::RemoveDotlock;
        }

        if self.state == State::RemoveDotlock {
            if self.lock.dotlock {
                let path = MboxLock::dotlock_path(&self.lock.path);
                return MboxCoroutineState::Yielded(MboxYield::WantsDotlockRemove(path));
            }
            self.state = State::Done;
        }

        debug!("mbox unlocked");
        MboxCoroutineState::Complete(Ok(()))
    }
}

#[derive(Debug, Eq, PartialEq)]
enum State {
    Start,
    UnlockFcntl,
    RemoveDotlock,
    Done,
}

impl fmt::Display for State {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Start => f.write_str("start"),
            Self::UnlockFcntl => f.write_str("unlock fcntl"),
            Self::RemoveDotlock => f.write_str("remove dotlock"),
            Self::Done => f.write_str("done"),
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::{coroutine::*, lock::MboxLock, lock::release::*, path::MboxFsPath};

    fn release(dotlock: bool, fcntl: bool) -> MboxLockRelease {
        MboxLockRelease::new(MboxLock {
            path: MboxFsPath::new("inbox"),
            dotlock,
            fcntl,
        })
    }

    #[test]
    fn releases_in_reverse_order() {
        let mut cor = release(true, true);
        assert!(matches!(
            cor.resume(None),
            MboxCoroutineState::Yielded(MboxYield::WantsFcntlUnlock(_))
        ));
        match cor.resume(Some(MboxReply::FcntlUnlock)) {
            MboxCoroutineState::Yielded(MboxYield::WantsDotlockRemove(path)) => {
                assert_eq!(path.as_str(), "inbox.lock")
            }
            state => panic!("unexpected {state:?}"),
        }
        assert!(matches!(
            cor.resume(Some(MboxReply::DotlockRemove)),
            MboxCoroutineState::Complete(Ok(()))
        ));
    }

    #[test]
    fn skips_locks_not_held() {
        assert!(matches!(
            release(false, false).resume(None),
            MboxCoroutineState::Complete(Ok(()))
        ));
        let mut cor = release(true, false);
        assert!(matches!(
            cor.resume(None),
            MboxCoroutineState::Yielded(MboxYield::WantsDotlockRemove(_))
        ));
        let mut cor = release(false, true);
        cor.resume(None);
        assert!(matches!(
            cor.resume(Some(MboxReply::FcntlUnlock)),
            MboxCoroutineState::Complete(Ok(()))
        ));
        assert!(matches!(
            cor.resume(Some(MboxReply::FcntlUnlock)),
            MboxCoroutineState::Complete(Err(_))
        ));
    }
}
