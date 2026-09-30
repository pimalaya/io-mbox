//! I/O-free coroutine appending messages to an mbox.
//!
//! The file is locked, padded so the previous message ends with a blank
//! line, then each message is written as a `From_` line, its header with
//! the flag fields replaced (plus a `Content-Length` for the mboxcl
//! variants), its quoted body and a separator. The whole batch goes out
//! in one positioned write at the end of the file.
//!
//! # Example
//!
//! ```rust,no_run
//! use io_mbox::{client::MboxClient, entry::append::MboxEntryAppendItem};
//!
//! let client = MboxClient::new("/path/to/mail");
//! let item = MboxEntryAppendItem {
//!     contents: b"Subject: hello\n\nworld\n".to_vec(),
//!     ..Default::default()
//! };
//! let entries = client.append("archive", vec![item]).unwrap();
//!
//! println!("appended {}", entries[0].id);
//! ```

use core::{fmt, mem};

use alloc::{string::String, vec::Vec};

use log::{debug, trace};
use thiserror::Error;

use crate::{
    coroutine::*,
    entry::MboxEntry,
    flag::{FLAG_FIELDS, MboxFlags},
    format::MboxFormat,
    from_line::MboxFromLine,
    header,
    lock::{MboxLock, MboxLockOptions, acquire::*, release::*},
    mbox_try,
    path::MboxFsPath,
    scan::{MboxScanner, MboxScannerOptions},
};

/// Failure causes of a [`MboxEntryAppend`] step.
#[derive(Clone, Debug, Error)]
pub enum MboxEntryAppendError {
    /// A reply arrived that does not match the awaited step.
    #[error("Mbox message append failed: unexpected arg {0:?}")]
    UnexpectedArg(Option<MboxReply>),
    /// The mbox does not exist.
    #[error("Mbox message append failed: {0} not found")]
    NotFound(MboxFsPath),
    /// The locks could not be taken.
    #[error(transparent)]
    Lock(#[from] MboxLockAcquireError),
    /// The locks could not be released.
    #[error(transparent)]
    Unlock(#[from] MboxLockReleaseError),
}

/// A message to append.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct MboxEntryAppendItem {
    /// The raw message, unquoted, without `From_` line.
    pub contents: Vec<u8>,
    /// Flags to store, replacing any flag field the message carries.
    pub flags: MboxFlags,
    /// Envelope sender for the `From_` line. Unset, it is taken from the
    /// `Return-Path` or `From` field, else `MAILER-DAEMON`.
    pub sender: Option<String>,
    /// Delivery date for the `From_` line, seconds since the Unix epoch.
    /// Unset, the current time.
    pub timestamp: Option<i64>,
}

/// Options of a [`MboxEntryAppend`].
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct MboxEntryAppendOptions {
    /// Format to write in, mboxrd when unset.
    pub format: MboxFormat,
    /// Locks to take.
    pub lock: MboxLockOptions,
    /// Scanner options the returned entries are produced with.
    pub scanner: MboxScannerOptions,
}

/// Appends messages to an mbox, under lock.
///
/// The returned entries carry content ids. A message already present
/// in the file gets the id of its first copy rather than a numbered one:
/// sync the index for exact ids.
#[derive(Debug)]
pub struct MboxEntryAppend {
    path: MboxFsPath,
    items: Vec<MboxEntryAppendItem>,
    opts: MboxEntryAppendOptions,
    lock: Option<MboxLock>,
    size: u64,
    entries: Vec<MboxEntry>,
    result: Option<Result<Vec<MboxEntry>, MboxEntryAppendError>>,
    state: State,
}

impl MboxEntryAppend {
    /// Builds a coroutine appending `items` to the mbox at `path`.
    pub fn new(
        path: MboxFsPath,
        items: Vec<MboxEntryAppendItem>,
        opts: MboxEntryAppendOptions,
    ) -> Self {
        Self {
            path,
            items,
            opts,
            lock: None,
            size: 0,
            entries: Vec::new(),
            result: None,
            state: State::Check,
        }
    }

    /// Releases the locks, then completes with `result`.
    fn finish(
        &mut self,
        result: Result<Vec<MboxEntry>, MboxEntryAppendError>,
    ) -> MboxCoroutineState<MboxYield, <Self as MboxCoroutine>::Return> {
        let Some(lock) = self.lock.take() else {
            self.state = State::Done;
            return MboxCoroutineState::Complete(result);
        };
        self.result = Some(result);
        self.state = State::Release(MboxLockRelease::new(lock));
        self.resume(None)
    }

    fn write(
        &mut self,
        tail: &[u8],
        now: i64,
    ) -> MboxCoroutineState<MboxYield, <Self as MboxCoroutine>::Return> {
        let mut bytes = Vec::new();
        if self.size > 0 {
            if !tail.ends_with(b"\n") {
                bytes.push(b'\n');
            }
            if !tail.ends_with(b"\n\n") && !tail.ends_with(b"\n\r\n") {
                bytes.push(b'\n');
            }
        }
        let pad = bytes.len();

        for item in mem::take(&mut self.items) {
            bytes.extend(render(item, self.opts.format, now));
        }

        let mut scanner = MboxScanner::new(self.size + pad as u64, self.opts.scanner.clone());
        self.entries = scanner.feed(&bytes[pad..]);
        self.entries.extend(scanner.finish());

        trace!("bytes: {}", bytes.len());
        self.state = State::Write;
        MboxCoroutineState::Yielded(MboxYield::WantsFileWrite {
            path: self.path.clone(),
            offset: self.size,
            bytes,
        })
    }
}

impl MboxCoroutine for MboxEntryAppend {
    type Yield = MboxYield;
    type Return = Result<Vec<MboxEntry>, MboxEntryAppendError>;

    fn resume(&mut self, arg: Option<MboxReply>) -> MboxCoroutineState<Self::Yield, Self::Return> {
        match (&mut self.state, arg) {
            (State::Check, None) => {
                self.state = State::Checking;
                MboxCoroutineState::Yielded(MboxYield::WantsFileMeta(self.path.clone()))
            }
            (State::Checking, Some(MboxReply::FileMeta(None))) => {
                let err = MboxEntryAppendError::NotFound(self.path.clone());
                self.finish(Err(err))
            }
            (State::Checking, Some(MboxReply::FileMeta(Some(_)))) => {
                let acquire = MboxLockAcquire::new(self.path.clone(), self.opts.lock.clone());
                self.state = State::Lock(acquire);
                self.resume(None)
            }
            (State::Lock(acquire), arg) => {
                self.lock = Some(mbox_try!(acquire, arg));
                self.state = State::Stat;
                MboxCoroutineState::Yielded(MboxYield::WantsFileMeta(self.path.clone()))
            }
            (State::Stat, Some(MboxReply::FileMeta(None))) => {
                let err = MboxEntryAppendError::NotFound(self.path.clone());
                self.finish(Err(err))
            }
            (State::Stat, Some(MboxReply::FileMeta(Some(meta)))) => {
                self.size = meta.size;
                let len = meta.size.min(3);
                self.state = State::ReadEnd;
                MboxCoroutineState::Yielded(MboxYield::WantsFileRead {
                    path: self.path.clone(),
                    offset: meta.size - len,
                    len: len as usize,
                })
            }
            (State::ReadEnd, Some(MboxReply::FileRead(tail))) => {
                self.state = State::Time(tail);
                MboxCoroutineState::Yielded(MboxYield::WantsTime)
            }
            (State::Time(tail), Some(MboxReply::Time { secs, .. })) => {
                let tail = mem::take(tail);
                self.write(&tail, secs as i64)
            }
            (State::Write, Some(MboxReply::FileWrite)) => {
                self.state = State::Sync;
                MboxCoroutineState::Yielded(MboxYield::WantsFileSync(self.path.clone()))
            }
            (State::Sync, Some(MboxReply::FileSync)) => {
                let entries = mem::take(&mut self.entries);
                debug!("appended mbox messages");
                trace!("count: {}", entries.len());
                self.finish(Ok(entries))
            }
            (State::Release(release), arg) => {
                mbox_try!(release, arg);
                self.state = State::Done;
                let result = self.result.take().unwrap_or(Ok(Vec::new()));
                MboxCoroutineState::Complete(result)
            }
            (_, arg) => self.finish(Err(MboxEntryAppendError::UnexpectedArg(arg))),
        }
    }
}

#[derive(Debug)]
enum State {
    Check,
    Checking,
    Lock(MboxLockAcquire),
    Stat,
    ReadEnd,
    Time(Vec<u8>),
    Write,
    Sync,
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
            Self::ReadEnd => f.write_str("read end"),
            Self::Time(_) => f.write_str("read time"),
            Self::Write => f.write_str("write messages"),
            Self::Sync => f.write_str("sync mbox"),
            Self::Release(_) => f.write_str("unlock mbox"),
            Self::Done => f.write_str("done"),
        }
    }
}

/// Renders one message as stored: `From_` line, header, quoted body and
/// separator.
pub(crate) fn render(item: MboxEntryAppendItem, format: MboxFormat, now: i64) -> Vec<u8> {
    let contents = item.contents;

    let header_end = header_end(&contents);
    let (head, rest) = contents.split_at(header_end);
    let crlf = head
        .split(|b| *b == b'\n')
        .next()
        .is_some_and(|line| line.ends_with(b"\r"));
    let eol: &[u8] = if crlf { b"\r\n" } else { b"\n" };

    let sender = item.sender.unwrap_or_else(|| {
        header::find(head, "Return-Path")
            .or_else(|| header::find(head, "From"))
            .and_then(|value| header::addr_spec(&value))
            .unwrap_or_default()
    });
    let timestamp = item.timestamp.unwrap_or(now);

    let mut dropped: Vec<&str> = FLAG_FIELDS.to_vec();
    if format.has_content_length() {
        dropped.push("content-length");
    }

    let mut head = strip_fields(head, &dropped);
    if !head.is_empty() && !head.ends_with(b"\n") {
        head.extend_from_slice(eol);
    }
    head.extend(item.flags.to_header(crlf));

    let mut body = if rest.is_empty() {
        eol.to_vec()
    } else {
        rest.to_vec()
    };
    if !body.ends_with(b"\n") {
        body.extend_from_slice(eol);
    }

    let mut out = MboxFromLine::format(&sender, timestamp);
    if crlf {
        out.pop();
        out.extend_from_slice(b"\r\n");
    }

    let body = format.escape(&body);
    out.extend(format.escape(&head));
    if format.has_content_length() {
        let blank = if body.starts_with(b"\r\n") { 2 } else { 1 };
        let len = body.len() - blank;
        out.extend_from_slice(format!("Content-Length: {len}").as_bytes());
        out.extend_from_slice(eol);
    }
    out.extend(body);
    out.extend_from_slice(eol);
    out
}

/// Offset where the header of `contents` ends: right after the newline
/// ending its last field, so the blank line starts the rest.
fn header_end(contents: &[u8]) -> usize {
    let mut offset = 0;
    for line in contents.split_inclusive(|b| *b == b'\n') {
        if header::trim_eol(line).is_empty() && line.ends_with(b"\n") {
            return offset;
        }
        offset += line.len();
    }
    contents.len()
}

/// Removes the fields named in `names` (lowercase) from a header block,
/// continuation lines included.
pub(crate) fn strip_fields(head: &[u8], names: &[&str]) -> Vec<u8> {
    let mut out = Vec::with_capacity(head.len());
    let mut dropping = false;
    for line in head.split_inclusive(|b| *b == b'\n') {
        if !matches!(line.first(), Some(b' ' | b'\t')) {
            dropping = header::field_name(line).is_some_and(|name| {
                let name = String::from_utf8_lossy(name).to_ascii_lowercase();
                names.contains(&name.as_str())
            });
        }
        if !dropping {
            out.extend_from_slice(line);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use alloc::vec::Vec;

    use crate::{
        coroutine::*,
        entry::append::*,
        flag::{MboxFlag, MboxFlags},
        format::MboxFormat,
        lock::MboxLockOptions,
        path::MboxFsPath,
    };

    fn item(contents: &[u8]) -> MboxEntryAppendItem {
        MboxEntryAppendItem {
            contents: contents.to_vec(),
            timestamp: Some(0),
            ..Default::default()
        }
    }

    fn unlocked() -> MboxEntryAppendOptions {
        MboxEntryAppendOptions {
            lock: MboxLockOptions {
                skip_dotlock: true,
                skip_fcntl: true,
                ..Default::default()
            },
            ..Default::default()
        }
    }

    /// Runs an append against an in-memory file.
    fn run(
        file: &mut Vec<u8>,
        items: Vec<MboxEntryAppendItem>,
        opts: MboxEntryAppendOptions,
    ) -> Result<Vec<MboxEntry>, MboxEntryAppendError> {
        let mut cor = MboxEntryAppend::new(MboxFsPath::new("mbox"), items, opts);
        let mut arg = None;
        loop {
            match cor.resume(arg.take()) {
                MboxCoroutineState::Complete(result) => return result,
                MboxCoroutineState::Yielded(MboxYield::WantsFileMeta(_)) => {
                    let meta = MboxFileMeta {
                        size: file.len() as u64,
                        ..Default::default()
                    };
                    arg = Some(MboxReply::FileMeta(Some(meta)));
                }
                MboxCoroutineState::Yielded(MboxYield::WantsFileRead { offset, len, .. }) => {
                    let start = offset as usize;
                    arg = Some(MboxReply::FileRead(file[start..start + len].to_vec()));
                }
                MboxCoroutineState::Yielded(MboxYield::WantsTime) => {
                    arg = Some(MboxReply::Time { secs: 0, nanos: 0 });
                }
                MboxCoroutineState::Yielded(MboxYield::WantsFileWrite {
                    offset, bytes, ..
                }) => {
                    assert_eq!(offset as usize, file.len());
                    file.extend(bytes);
                    arg = Some(MboxReply::FileWrite);
                }
                MboxCoroutineState::Yielded(MboxYield::WantsFileSync(_)) => {
                    arg = Some(MboxReply::FileSync);
                }
                state => panic!("unexpected {state:?}"),
            }
        }
    }

    #[test]
    fn appends_to_an_empty_file() {
        let mut file = Vec::new();
        let mut one = item(b"From: A <a@b.c>\nSubject: x\nStatus: RO\n\nFrom here\n");
        one.flags = MboxFlags::from_iter([MboxFlag::Flagged]);
        let entries = run(
            &mut file,
            vec![one, item(b"Subject: y\n\nno newline")],
            unlocked(),
        )
        .unwrap();
        assert_eq!(
            file,
            b"From a@b.c Thu Jan  1 00:00:00 1970\nFrom: A <a@b.c>\nSubject: x\nX-Status: F\n\n>From here\n\n\
From MAILER-DAEMON Thu Jan  1 00:00:00 1970\nSubject: y\n\nno newline\n\n"
                .as_slice()
        );
        assert_eq!(entries.len(), 2);
        assert!(entries[0].flags.contains(&MboxFlag::Flagged));
        assert_eq!(entries[1].offset, 88);
    }

    #[test]
    fn pads_the_previous_message() {
        for (before, pad) in [
            (b"x".as_slice(), b"\n\n".as_slice()),
            (b"x\n", b"\n"),
            (b"x\n\n", b""),
            (b"x\r\n\r\n", b""),
        ] {
            let mut file = before.to_vec();
            let entries = run(&mut file, vec![item(b"Subject: x\n\nbody\n")], unlocked()).unwrap();
            let offset = before.len() + pad.len();
            assert_eq!(&file[before.len()..offset], pad);
            assert_eq!(entries[0].offset as usize, offset);
        }
    }

    #[test]
    fn content_length_variants() {
        let mut file = Vec::new();
        let opts = MboxEntryAppendOptions {
            format: MboxFormat::MboxCl2,
            ..unlocked()
        };
        run(
            &mut file,
            vec![item(b"Content-Length: 99\nReturn-Path: <r@s>\n\nFrom x\n")],
            opts,
        )
        .unwrap();
        assert_eq!(
            file,
            b"From r@s Thu Jan  1 00:00:00 1970\nReturn-Path: <r@s>\nContent-Length: 7\n\nFrom x\n\n".as_slice()
        );

        let mut file = Vec::new();
        let opts = MboxEntryAppendOptions {
            format: MboxFormat::MboxCl,
            ..unlocked()
        };
        run(&mut file, vec![item(b"Subject: none")], opts).unwrap();
        assert_eq!(
            file,
            b"From MAILER-DAEMON Thu Jan  1 00:00:00 1970\nSubject: none\nContent-Length: 0\n\n\n"
                .as_slice()
        );
    }

    #[test]
    fn crlf_messages_keep_crlf() {
        let mut file = Vec::new();
        let mut one = item(b"Subject: x\r\n\r\nbody\r\n");
        one.flags = MboxFlags::from_iter([MboxFlag::Seen]);
        let entries = run(&mut file, vec![one], unlocked()).unwrap();
        assert_eq!(
            file,
            b"From MAILER-DAEMON Thu Jan  1 00:00:00 1970\r\nSubject: x\r\nStatus: R\r\n\r\nbody\r\n\r\n".as_slice()
        );
        assert!(entries[0].crlf);
    }

    #[test]
    fn missing_file_releases_the_locks() {
        let opts = MboxEntryAppendOptions {
            lock: MboxLockOptions {
                skip_dotlock: true,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut cor = MboxEntryAppend::new(MboxFsPath::new("mbox"), Vec::new(), opts.clone());
        cor.resume(None);
        assert!(matches!(
            cor.resume(Some(MboxReply::FileMeta(None))),
            MboxCoroutineState::Complete(Err(MboxEntryAppendError::NotFound(_)))
        ));

        let mut cor = MboxEntryAppend::new(MboxFsPath::new("mbox"), Vec::new(), opts);
        cor.resume(None);
        let state = cor.resume(Some(MboxReply::FileMeta(Some(Default::default()))));
        assert!(matches!(
            state,
            MboxCoroutineState::Yielded(MboxYield::WantsFcntlLock(_))
        ));
        cor.resume(Some(MboxReply::FcntlLock(true)));
        let state = cor.resume(Some(MboxReply::FileMeta(None)));
        assert!(matches!(
            state,
            MboxCoroutineState::Yielded(MboxYield::WantsFcntlUnlock(_))
        ));
        let state = cor.resume(Some(MboxReply::FcntlUnlock));
        assert!(matches!(
            state,
            MboxCoroutineState::Complete(Err(MboxEntryAppendError::NotFound(_)))
        ));
    }

    #[test]
    fn unexpected_replies_release_the_locks() {
        let mut cor = MboxEntryAppend::new(MboxFsPath::new("mbox"), Vec::new(), unlocked());
        cor.resume(None);
        let state = cor.resume(Some(MboxReply::Sleep));
        assert!(matches!(
            state,
            MboxCoroutineState::Complete(Err(MboxEntryAppendError::UnexpectedArg(_)))
        ));
    }
}
