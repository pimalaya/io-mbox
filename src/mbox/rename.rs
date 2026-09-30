//! I/O-free coroutine renaming an mbox file.
//!
//! Only the file moves: in the Thunderbird layout, the `.sbd` directory
//! holding its children keeps its name.

use core::fmt;

use log::debug;
use thiserror::Error;

use crate::{coroutine::*, path::MboxFsPath};

/// Failure causes of a [`MboxRename`] step.
#[derive(Clone, Debug, Error)]
pub enum MboxRenameError {
    /// A reply arrived that does not match the awaited step.
    #[error("Mbox rename failed: unexpected arg {0:?}")]
    UnexpectedArg(Option<MboxReply>),
    /// The source does not exist.
    #[error("Mbox rename failed: {0} not found")]
    NotFound(MboxFsPath),
    /// The target already exists.
    #[error("Mbox rename failed: {0} already exists")]
    AlreadyExists(MboxFsPath),
}

/// Renames an mbox file, refusing to overwrite another.
#[derive(Debug)]
pub struct MboxRename {
    from: MboxFsPath,
    to: MboxFsPath,
    state: State,
}

impl MboxRename {
    /// Builds a coroutine renaming the mbox at `from` to `to`.
    pub fn new(from: MboxFsPath, to: MboxFsPath) -> Self {
        Self {
            from,
            to,
            state: State::Start,
        }
    }
}

impl MboxCoroutine for MboxRename {
    type Yield = MboxYield;
    type Return = Result<(), MboxRenameError>;

    fn resume(&mut self, arg: Option<MboxReply>) -> MboxCoroutineState<Self::Yield, Self::Return> {
        match (&self.state, arg) {
            (State::Start, None) => {
                self.state = State::StatFrom;
                MboxCoroutineState::Yielded(MboxYield::WantsFileMeta(self.from.clone()))
            }
            (State::StatFrom, Some(MboxReply::FileMeta(None))) => {
                MboxCoroutineState::Complete(Err(MboxRenameError::NotFound(self.from.clone())))
            }
            (State::StatFrom, Some(MboxReply::FileMeta(Some(_)))) => {
                self.state = State::StatTo;
                MboxCoroutineState::Yielded(MboxYield::WantsFileMeta(self.to.clone()))
            }
            (State::StatTo, Some(MboxReply::FileMeta(Some(_)))) => {
                MboxCoroutineState::Complete(Err(MboxRenameError::AlreadyExists(self.to.clone())))
            }
            (State::StatTo, Some(MboxReply::FileMeta(None))) => match self.to.parent() {
                Some(parent) => {
                    self.state = State::CreateDir;
                    MboxCoroutineState::Yielded(MboxYield::WantsDirCreate(parent))
                }
                None => {
                    self.state = State::CreateDir;
                    self.resume(Some(MboxReply::DirCreate))
                }
            },
            (State::CreateDir, Some(MboxReply::DirCreate)) => {
                self.state = State::Rename;
                MboxCoroutineState::Yielded(MboxYield::WantsRename {
                    from: self.from.clone(),
                    to: self.to.clone(),
                })
            }
            (State::Rename, Some(MboxReply::Rename)) => {
                debug!("mbox renamed");
                self.state = State::Done;
                MboxCoroutineState::Complete(Ok(()))
            }
            (_, arg) => MboxCoroutineState::Complete(Err(MboxRenameError::UnexpectedArg(arg))),
        }
    }
}

#[derive(Debug)]
enum State {
    Start,
    StatFrom,
    StatTo,
    CreateDir,
    Rename,
    Done,
}

impl fmt::Display for State {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Start => f.write_str("start"),
            Self::StatFrom => f.write_str("stat source"),
            Self::StatTo => f.write_str("stat target"),
            Self::CreateDir => f.write_str("create parent"),
            Self::Rename => f.write_str("rename file"),
            Self::Done => f.write_str("done"),
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::{coroutine::*, mbox::rename::*, path::MboxFsPath};

    fn meta(exists: bool) -> Option<MboxReply> {
        Some(MboxReply::FileMeta(exists.then(MboxFileMeta::default)))
    }

    #[test]
    fn renames_into_a_new_parent() {
        let mut cor = MboxRename::new(MboxFsPath::new("a"), MboxFsPath::new("b"));
        cor.resume(None);
        cor.resume(meta(true));
        assert!(matches!(
            cor.resume(meta(false)),
            MboxCoroutineState::Yielded(MboxYield::WantsRename { .. })
        ));
        assert!(matches!(
            cor.resume(Some(MboxReply::Rename)),
            MboxCoroutineState::Complete(Ok(()))
        ));

        let mut cor = MboxRename::new(MboxFsPath::new("a"), MboxFsPath::new("d/b"));
        cor.resume(None);
        cor.resume(meta(true));
        assert!(matches!(
            cor.resume(meta(false)),
            MboxCoroutineState::Yielded(MboxYield::WantsDirCreate(_))
        ));
    }

    #[test]
    fn refuses_missing_sources_and_existing_targets() {
        let mut cor = MboxRename::new(MboxFsPath::new("a"), MboxFsPath::new("b"));
        cor.resume(None);
        assert!(matches!(
            cor.resume(meta(false)),
            MboxCoroutineState::Complete(Err(MboxRenameError::NotFound(_)))
        ));

        let mut cor = MboxRename::new(MboxFsPath::new("a"), MboxFsPath::new("b"));
        cor.resume(None);
        cor.resume(meta(true));
        assert!(matches!(
            cor.resume(meta(true)),
            MboxCoroutineState::Complete(Err(MboxRenameError::AlreadyExists(_)))
        ));
        assert!(matches!(
            cor.resume(None),
            MboxCoroutineState::Complete(Err(MboxRenameError::UnexpectedArg(None)))
        ));
    }
}
