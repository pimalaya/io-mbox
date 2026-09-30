//! I/O-free coroutine taking the dotlock and the fcntl lock of an mbox.

use core::fmt;

use alloc::boxed::Box;

use log::{debug, trace};
use thiserror::Error;

use crate::{
    coroutine::*,
    lock::{MboxLock, MboxLockOptions, RETRY_MS, STALE_SECS, TIMEOUT_SECS},
    path::MboxFsPath,
};

/// Failure causes of a [`MboxLockAcquire`] step.
#[derive(Clone, Debug, Error)]
pub enum MboxLockAcquireError {
    /// A reply arrived that does not match the awaited step.
    #[error("Mbox lock failed: unexpected arg {0:?}")]
    UnexpectedArg(Option<MboxReply>),
    /// Another process kept the dotlock past the timeout.
    #[error("Mbox lock failed: {0} is held by another process")]
    DotlockBusy(MboxFsPath),
    /// Another process kept the fcntl lock past the timeout.
    #[error("Mbox lock failed: {0} is locked by another process")]
    FcntlBusy(MboxFsPath),
}

/// Takes the locks of an mbox, retrying while another process holds
/// them.
#[derive(Debug)]
pub struct MboxLockAcquire {
    lock: MboxLock,
    opts: MboxLockOptions,
    attempts: u64,
    state: State,
}

impl MboxLockAcquire {
    /// Builds a coroutine locking the mbox at `path`.
    pub fn new(path: MboxFsPath, opts: MboxLockOptions) -> Self {
        Self {
            lock: MboxLock {
                path,
                dotlock: false,
                fcntl: false,
            },
            opts,
            attempts: 0,
            state: State::Start,
        }
    }

    fn max_attempts(&self) -> u64 {
        let timeout = self.opts.timeout_secs.unwrap_or(TIMEOUT_SECS);
        (timeout * 1000 / RETRY_MS).max(1)
    }

    fn dotlock(&mut self) -> MboxCoroutineState<MboxYield, <Self as MboxCoroutine>::Return> {
        self.state = State::CreateDotlock;
        let path = MboxLock::dotlock_path(&self.lock.path);
        MboxCoroutineState::Yielded(MboxYield::WantsDotlockCreate(path))
    }

    fn fcntl(&mut self) -> MboxCoroutineState<MboxYield, <Self as MboxCoroutine>::Return> {
        if self.opts.skip_fcntl {
            return self.done();
        }
        self.attempts = 0;
        self.state = State::LockFcntl;
        MboxCoroutineState::Yielded(MboxYield::WantsFcntlLock(self.lock.path.clone()))
    }

    fn done(&mut self) -> MboxCoroutineState<MboxYield, <Self as MboxCoroutine>::Return> {
        debug!("mbox locked");
        trace!("lock: {:?}", self.lock);
        self.state = State::Done;
        MboxCoroutineState::Complete(Ok(self.lock.clone()))
    }

    /// Sleeps before the next attempt, or gives up with `err`.
    fn retry(
        &mut self,
        next: State,
        err: MboxLockAcquireError,
    ) -> MboxCoroutineState<MboxYield, <Self as MboxCoroutine>::Return> {
        self.attempts += 1;
        if self.attempts >= self.max_attempts() {
            if self.lock.dotlock {
                self.state = State::Abort(err);
                let path = MboxLock::dotlock_path(&self.lock.path);
                return MboxCoroutineState::Yielded(MboxYield::WantsDotlockRemove(path));
            }
            return MboxCoroutineState::Complete(Err(err));
        }
        self.state = State::Sleep(next.into());
        MboxCoroutineState::Yielded(MboxYield::WantsSleep(RETRY_MS))
    }
}

impl MboxCoroutine for MboxLockAcquire {
    type Yield = MboxYield;
    type Return = Result<MboxLock, MboxLockAcquireError>;

    fn resume(&mut self, arg: Option<MboxReply>) -> MboxCoroutineState<Self::Yield, Self::Return> {
        let dotlock = MboxLock::dotlock_path(&self.lock.path);

        match (&mut self.state, arg) {
            (State::Start, None) if self.opts.skip_dotlock => self.fcntl(),
            (State::Start, None) => self.dotlock(),
            (State::CreateDotlock, Some(MboxReply::DotlockCreate(true))) => {
                self.lock.dotlock = true;
                self.fcntl()
            }
            (State::CreateDotlock, Some(MboxReply::DotlockCreate(false))) => {
                self.state = State::StatDotlock;
                MboxCoroutineState::Yielded(MboxYield::WantsFileMeta(dotlock))
            }
            (State::StatDotlock, Some(MboxReply::FileMeta(None))) => self.dotlock(),
            (State::StatDotlock, Some(MboxReply::FileMeta(Some(meta)))) => {
                self.state = State::CheckStale(meta.mtime_secs);
                MboxCoroutineState::Yielded(MboxYield::WantsTime)
            }
            (State::CheckStale(mtime), Some(MboxReply::Time { secs, .. })) => {
                let stale = self.opts.stale_secs.unwrap_or(STALE_SECS) as i64;
                if secs as i64 - *mtime > stale {
                    debug!("break stale dotlock");
                    trace!("path: {dotlock}");
                    self.state = State::BreakDotlock;
                    return MboxCoroutineState::Yielded(MboxYield::WantsFileRemove(dotlock));
                }
                self.retry(
                    State::CreateDotlock,
                    MboxLockAcquireError::DotlockBusy(dotlock),
                )
            }
            (State::BreakDotlock, Some(MboxReply::FileRemove)) => self.dotlock(),
            (State::LockFcntl, Some(MboxReply::FcntlLock(true))) => {
                self.lock.fcntl = true;
                self.done()
            }
            (State::LockFcntl, Some(MboxReply::FcntlLock(false))) => {
                let err = MboxLockAcquireError::FcntlBusy(self.lock.path.clone());
                self.retry(State::LockFcntl, err)
            }
            (State::Sleep(next), Some(MboxReply::Sleep)) => match **next {
                State::LockFcntl => {
                    self.state = State::LockFcntl;
                    MboxCoroutineState::Yielded(MboxYield::WantsFcntlLock(self.lock.path.clone()))
                }
                _ => self.dotlock(),
            },
            (State::Abort(err), Some(MboxReply::DotlockRemove)) => {
                let err = err.clone();
                self.lock.dotlock = false;
                MboxCoroutineState::Complete(Err(err))
            }
            (_, arg) => MboxCoroutineState::Complete(Err(MboxLockAcquireError::UnexpectedArg(arg))),
        }
    }
}

#[derive(Debug)]
enum State {
    Start,
    CreateDotlock,
    StatDotlock,
    CheckStale(i64),
    BreakDotlock,
    LockFcntl,
    Sleep(Box<State>),
    Abort(MboxLockAcquireError),
    Done,
}

impl fmt::Display for State {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Start => f.write_str("start"),
            Self::CreateDotlock => f.write_str("create dotlock"),
            Self::StatDotlock => f.write_str("stat dotlock"),
            Self::CheckStale(_) => f.write_str("check stale dotlock"),
            Self::BreakDotlock => f.write_str("break dotlock"),
            Self::LockFcntl => f.write_str("lock fcntl"),
            Self::Sleep(_) => f.write_str("sleep"),
            Self::Abort(_) => f.write_str("abort"),
            Self::Done => f.write_str("done"),
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::{coroutine::*, lock::acquire::*, path::MboxFsPath};

    fn path() -> MboxFsPath {
        MboxFsPath::new("inbox")
    }

    fn lock(opts: MboxLockOptions) -> MboxLockAcquire {
        MboxLockAcquire::new(path(), opts)
    }

    fn expect(cor: &mut MboxLockAcquire, arg: Option<MboxReply>, want: MboxYield) {
        match cor.resume(arg) {
            MboxCoroutineState::Yielded(y) => assert_eq!(y, want),
            state => panic!("expected {want:?}, got {state:?}"),
        }
    }

    fn expect_complete(
        cor: &mut MboxLockAcquire,
        arg: Option<MboxReply>,
    ) -> Result<MboxLock, MboxLockAcquireError> {
        match cor.resume(arg) {
            MboxCoroutineState::Complete(result) => result,
            state => panic!("expected Complete, got {state:?}"),
        }
    }

    #[test]
    fn takes_both_locks() {
        let mut cor = lock(Default::default());
        expect(
            &mut cor,
            None,
            MboxYield::WantsDotlockCreate("inbox.lock".into()),
        );
        expect(
            &mut cor,
            Some(MboxReply::DotlockCreate(true)),
            MboxYield::WantsFcntlLock(path()),
        );
        let lock = expect_complete(&mut cor, Some(MboxReply::FcntlLock(true))).unwrap();
        assert!(lock.dotlock && lock.fcntl);
    }

    #[test]
    fn skips_what_it_is_told() {
        let opts = MboxLockOptions {
            skip_dotlock: true,
            ..Default::default()
        };
        let mut cor = lock(opts);
        expect(&mut cor, None, MboxYield::WantsFcntlLock(path()));
        let lock = expect_complete(&mut cor, Some(MboxReply::FcntlLock(true))).unwrap();
        assert!(!lock.dotlock && lock.fcntl);

        let opts = MboxLockOptions {
            skip_dotlock: true,
            skip_fcntl: true,
            ..Default::default()
        };
        let lock = expect_complete(&mut self::lock(opts), None).unwrap();
        assert!(!lock.dotlock && !lock.fcntl);
    }

    #[test]
    fn waits_for_a_busy_dotlock_then_breaks_it_when_stale() {
        let mut cor = lock(Default::default());
        let dotlock = MboxFsPath::new("inbox.lock");
        expect(
            &mut cor,
            None,
            MboxYield::WantsDotlockCreate(dotlock.clone()),
        );
        expect(
            &mut cor,
            Some(MboxReply::DotlockCreate(false)),
            MboxYield::WantsFileMeta(dotlock.clone()),
        );
        let meta = MboxFileMeta {
            mtime_secs: 1000,
            ..Default::default()
        };
        expect(
            &mut cor,
            Some(MboxReply::FileMeta(Some(meta))),
            MboxYield::WantsTime,
        );
        expect(
            &mut cor,
            Some(MboxReply::Time {
                secs: 1010,
                nanos: 0,
            }),
            MboxYield::WantsSleep(100),
        );
        expect(
            &mut cor,
            Some(MboxReply::Sleep),
            MboxYield::WantsDotlockCreate(dotlock.clone()),
        );
        expect(
            &mut cor,
            Some(MboxReply::DotlockCreate(false)),
            MboxYield::WantsFileMeta(dotlock.clone()),
        );
        expect(
            &mut cor,
            Some(MboxReply::FileMeta(None)),
            MboxYield::WantsDotlockCreate(dotlock.clone()),
        );
        expect(
            &mut cor,
            Some(MboxReply::DotlockCreate(false)),
            MboxYield::WantsFileMeta(dotlock.clone()),
        );
        expect(
            &mut cor,
            Some(MboxReply::FileMeta(Some(meta))),
            MboxYield::WantsTime,
        );
        expect(
            &mut cor,
            Some(MboxReply::Time {
                secs: 2000,
                nanos: 0,
            }),
            MboxYield::WantsFileRemove(dotlock.clone()),
        );
        expect(
            &mut cor,
            Some(MboxReply::FileRemove),
            MboxYield::WantsDotlockCreate(dotlock),
        );
    }

    #[test]
    fn gives_up_and_releases_the_dotlock() {
        let opts = MboxLockOptions {
            timeout_secs: Some(0),
            ..Default::default()
        };
        let mut cor = lock(opts);
        let dotlock = MboxFsPath::new("inbox.lock");
        expect(
            &mut cor,
            None,
            MboxYield::WantsDotlockCreate(dotlock.clone()),
        );
        expect(
            &mut cor,
            Some(MboxReply::DotlockCreate(true)),
            MboxYield::WantsFcntlLock(path()),
        );
        expect(
            &mut cor,
            Some(MboxReply::FcntlLock(false)),
            MboxYield::WantsDotlockRemove(dotlock),
        );
        let err = expect_complete(&mut cor, Some(MboxReply::DotlockRemove)).unwrap_err();
        assert!(matches!(err, MboxLockAcquireError::FcntlBusy(_)));

        let opts = MboxLockOptions {
            timeout_secs: Some(0),
            skip_dotlock: true,
            ..Default::default()
        };
        let mut cor = lock(opts);
        expect(&mut cor, None, MboxYield::WantsFcntlLock(path()));
        let err = expect_complete(&mut cor, Some(MboxReply::FcntlLock(false))).unwrap_err();
        assert!(matches!(err, MboxLockAcquireError::FcntlBusy(_)));
    }

    #[test]
    fn retries_a_busy_fcntl_lock() {
        let opts = MboxLockOptions {
            skip_dotlock: true,
            ..Default::default()
        };
        let mut cor = lock(opts);
        expect(&mut cor, None, MboxYield::WantsFcntlLock(path()));
        expect(
            &mut cor,
            Some(MboxReply::FcntlLock(false)),
            MboxYield::WantsSleep(100),
        );
        expect(
            &mut cor,
            Some(MboxReply::Sleep),
            MboxYield::WantsFcntlLock(path()),
        );
        assert!(expect_complete(&mut cor, Some(MboxReply::FcntlLock(true))).is_ok());
    }

    #[test]
    fn busy_dotlock_times_out() {
        let opts = MboxLockOptions {
            timeout_secs: Some(0),
            ..Default::default()
        };
        let mut cor = lock(opts);
        let dotlock = MboxFsPath::new("inbox.lock");
        expect(
            &mut cor,
            None,
            MboxYield::WantsDotlockCreate(dotlock.clone()),
        );
        expect(
            &mut cor,
            Some(MboxReply::DotlockCreate(false)),
            MboxYield::WantsFileMeta(dotlock),
        );
        expect(
            &mut cor,
            Some(MboxReply::FileMeta(Some(Default::default()))),
            MboxYield::WantsTime,
        );
        let err =
            expect_complete(&mut cor, Some(MboxReply::Time { secs: 0, nanos: 0 })).unwrap_err();
        assert!(matches!(err, MboxLockAcquireError::DotlockBusy(_)));
        assert!(matches!(
            expect_complete(&mut cor, None),
            Err(MboxLockAcquireError::UnexpectedArg(None))
        ));
    }
}
