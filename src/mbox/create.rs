//! I/O-free coroutine creating an empty mbox file.

use core::fmt;

use log::debug;
use thiserror::Error;

use crate::{coroutine::*, path::MboxFsPath};

/// Failure causes of a [`MboxCreate`] step.
#[derive(Clone, Debug, Error)]
pub enum MboxCreateError {
    /// A reply arrived that does not match the awaited step.
    #[error("Mbox create failed: unexpected arg {0:?}")]
    UnexpectedArg(Option<MboxReply>),
    /// A file already exists at the path.
    #[error("Mbox create failed: {0} already exists")]
    AlreadyExists(MboxFsPath),
}

/// Creates an empty mbox file, and its parent directories.
#[derive(Debug)]
pub struct MboxCreate {
    path: MboxFsPath,
    state: State,
}

impl MboxCreate {
    /// Builds a coroutine creating the mbox at `path`.
    pub fn new(path: MboxFsPath) -> Self {
        Self {
            path,
            state: State::Start,
        }
    }
}

impl MboxCoroutine for MboxCreate {
    type Yield = MboxYield;
    type Return = Result<(), MboxCreateError>;

    fn resume(&mut self, arg: Option<MboxReply>) -> MboxCoroutineState<Self::Yield, Self::Return> {
        match (&self.state, arg) {
            (State::Start, None) => match self.path.parent() {
                Some(parent) => {
                    self.state = State::CreateDir;
                    MboxCoroutineState::Yielded(MboxYield::WantsDirCreate(parent))
                }
                None => self.resume(Some(MboxReply::DirCreate)),
            },
            (State::Start | State::CreateDir, Some(MboxReply::DirCreate)) => {
                self.state = State::CreateFile;
                MboxCoroutineState::Yielded(MboxYield::WantsFileCreate(self.path.clone()))
            }
            (State::CreateFile, Some(MboxReply::FileCreate(true))) => {
                debug!("mbox created");
                self.state = State::Done;
                MboxCoroutineState::Complete(Ok(()))
            }
            (State::CreateFile, Some(MboxReply::FileCreate(false))) => {
                let err = MboxCreateError::AlreadyExists(self.path.clone());
                MboxCoroutineState::Complete(Err(err))
            }
            (_, arg) => MboxCoroutineState::Complete(Err(MboxCreateError::UnexpectedArg(arg))),
        }
    }
}

#[derive(Debug)]
enum State {
    Start,
    CreateDir,
    CreateFile,
    Done,
}

impl fmt::Display for State {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Start => f.write_str("start"),
            Self::CreateDir => f.write_str("create parent"),
            Self::CreateFile => f.write_str("create file"),
            Self::Done => f.write_str("done"),
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::{coroutine::*, mbox::create::*, path::MboxFsPath};

    #[test]
    fn creates_parent_then_file() {
        let mut cor = MboxCreate::new(MboxFsPath::new("root/a"));
        assert!(matches!(
            cor.resume(None),
            MboxCoroutineState::Yielded(MboxYield::WantsDirCreate(p)) if p.as_str() == "root"
        ));
        assert!(matches!(
            cor.resume(Some(MboxReply::DirCreate)),
            MboxCoroutineState::Yielded(MboxYield::WantsFileCreate(_))
        ));
        assert!(matches!(
            cor.resume(Some(MboxReply::FileCreate(true))),
            MboxCoroutineState::Complete(Ok(()))
        ));
    }

    #[test]
    fn refuses_existing_files() {
        let mut cor = MboxCreate::new(MboxFsPath::new("a"));
        assert!(matches!(
            cor.resume(None),
            MboxCoroutineState::Yielded(MboxYield::WantsFileCreate(_))
        ));
        assert!(matches!(
            cor.resume(Some(MboxReply::FileCreate(false))),
            MboxCoroutineState::Complete(Err(MboxCreateError::AlreadyExists(_)))
        ));
        assert!(matches!(
            cor.resume(None),
            MboxCoroutineState::Complete(Err(MboxCreateError::UnexpectedArg(None)))
        ));
    }
}
