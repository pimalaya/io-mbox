//! I/O-free coroutine copying a byte range between two files, chunk by
//! chunk, so a rewrite never holds more than one chunk in memory.
//!
//! [`crate::entry::update`] copies the unchanged parts of an mbox to a
//! temporary file and back with it.

use core::fmt;

use alloc::vec::Vec;

use thiserror::Error;

use crate::{coroutine::*, path::MboxFsPath};

/// Failure causes of a [`MboxRangeCopy`] step.
#[derive(Clone, Debug, Error)]
pub enum MboxRangeCopyError {
    /// A reply arrived that does not match the awaited step.
    #[error("Mbox range copy failed: unexpected arg {0:?}")]
    UnexpectedArg(Option<MboxReply>),
    /// The source ended before the range did.
    #[error("Mbox range copy failed: {path} ended at {offset}, before the range did")]
    ShortRead {
        /// The source file.
        path: MboxFsPath,
        /// Where it ended.
        offset: u64,
    },
}

/// A byte range of a file.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct MboxRange {
    /// The file.
    pub path: MboxFsPath,
    /// Start of the range.
    pub offset: u64,
}

/// Copies `len` bytes from one file range to another.
#[derive(Debug)]
pub struct MboxRangeCopy {
    src: MboxRange,
    dst: MboxRange,
    remaining: u64,
    chunk_size: usize,
    state: State,
}

impl MboxRangeCopy {
    /// Builds a coroutine copying `len` bytes from `src` to `dst`, reading
    /// `chunk_size` bytes at a time.
    pub fn new(src: MboxRange, dst: MboxRange, len: u64, chunk_size: usize) -> Self {
        Self {
            src,
            dst,
            remaining: len,
            chunk_size: chunk_size.max(1),
            state: State::Read,
        }
    }

    fn read(&mut self) -> MboxCoroutineState<MboxYield, <Self as MboxCoroutine>::Return> {
        if self.remaining == 0 {
            self.state = State::Done;
            return MboxCoroutineState::Complete(Ok(()));
        }

        self.state = State::Write;
        MboxCoroutineState::Yielded(MboxYield::WantsFileRead {
            path: self.src.path.clone(),
            offset: self.src.offset,
            len: (self.chunk_size as u64).min(self.remaining) as usize,
        })
    }

    fn write(
        &mut self,
        bytes: Vec<u8>,
    ) -> MboxCoroutineState<MboxYield, <Self as MboxCoroutine>::Return> {
        if bytes.is_empty() {
            let err = MboxRangeCopyError::ShortRead {
                path: self.src.path.clone(),
                offset: self.src.offset,
            };
            return MboxCoroutineState::Complete(Err(err));
        }

        let len = (bytes.len() as u64).min(self.remaining);
        let mut bytes = bytes;
        bytes.truncate(len as usize);

        let offset = self.dst.offset;
        self.src.offset += len;
        self.dst.offset += len;
        self.remaining -= len;
        self.state = State::Read;

        MboxCoroutineState::Yielded(MboxYield::WantsFileWrite {
            path: self.dst.path.clone(),
            offset,
            bytes,
        })
    }
}

impl MboxCoroutine for MboxRangeCopy {
    type Yield = MboxYield;
    type Return = Result<(), MboxRangeCopyError>;

    fn resume(&mut self, arg: Option<MboxReply>) -> MboxCoroutineState<Self::Yield, Self::Return> {
        match (&self.state, arg) {
            (State::Read, None | Some(MboxReply::FileWrite)) => self.read(),
            (State::Write, Some(MboxReply::FileRead(bytes))) => self.write(bytes),
            (_, arg) => MboxCoroutineState::Complete(Err(MboxRangeCopyError::UnexpectedArg(arg))),
        }
    }
}

#[derive(Debug)]
enum State {
    Read,
    Write,
    Done,
}

impl fmt::Display for State {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Read => f.write_str("read chunk"),
            Self::Write => f.write_str("write chunk"),
            Self::Done => f.write_str("done"),
        }
    }
}

#[cfg(test)]
mod tests {
    use alloc::vec::Vec;

    use crate::{coroutine::*, path::MboxFsPath, range::*};

    fn range(path: &str, offset: u64) -> MboxRange {
        MboxRange {
            path: MboxFsPath::new(path),
            offset,
        }
    }

    #[test]
    fn copies_in_chunks() {
        let src = b"0123456789";
        let mut dst = Vec::new();
        let mut cor = MboxRangeCopy::new(range("src", 2), range("dst", 0), 7, 3);
        let mut arg = None;
        loop {
            match cor.resume(arg.take()) {
                MboxCoroutineState::Complete(result) => break result.unwrap(),
                MboxCoroutineState::Yielded(MboxYield::WantsFileRead { offset, len, .. }) => {
                    let start = offset as usize;
                    arg = Some(MboxReply::FileRead(
                        src[start..(start + len + 1).min(10)].to_vec(),
                    ));
                }
                MboxCoroutineState::Yielded(MboxYield::WantsFileWrite {
                    offset, bytes, ..
                }) => {
                    assert_eq!(offset as usize, dst.len());
                    dst.extend(bytes);
                    arg = Some(MboxReply::FileWrite);
                }
                state => panic!("unexpected {state:?}"),
            }
        }
        assert_eq!(dst, b"2345678");
    }

    #[test]
    fn short_reads_and_bad_replies_fail() {
        let mut cor = MboxRangeCopy::new(range("src", 0), range("dst", 0), 4, 3);
        cor.resume(None);
        let state = cor.resume(Some(MboxReply::FileRead(Vec::new())));
        assert!(matches!(
            state,
            MboxCoroutineState::Complete(Err(MboxRangeCopyError::ShortRead { .. }))
        ));

        let mut cor = MboxRangeCopy::new(range("src", 0), range("dst", 0), 0, 3);
        assert!(matches!(
            cor.resume(None),
            MboxCoroutineState::Complete(Ok(()))
        ));
        assert!(matches!(
            cor.resume(None),
            MboxCoroutineState::Complete(Err(MboxRangeCopyError::UnexpectedArg(None)))
        ));
    }
}
