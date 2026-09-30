//! I/O-free coroutine listing the mbox files of a store.
//!
//! The root is walked recursively. Every regular file is a mailbox, except
//! hidden files and the side files of other tools (`.lock` dotlocks,
//! Thunderbird `.msf` summaries). The spool, when set, is listed as
//! `INBOX` if it exists.

use core::{fmt, mem};

use alloc::{
    collections::{BTreeSet, VecDeque},
    vec::Vec,
};

use log::{debug, trace};
use thiserror::Error;

use crate::{
    coroutine::*,
    mbox::Mbox,
    path::{MboxFsPath, MboxPath},
    store::{INBOX, MboxStore},
};

/// Suffixes of files that are not mailboxes.
const IGNORED_SUFFIXES: [&str; 3] = [".lock", ".msf", ".lck"];

/// Failure causes of a [`MboxList`] step.
#[derive(Clone, Debug, Error)]
pub enum MboxListError {
    /// A reply arrived that does not match the awaited step.
    #[error("Mbox list failed: unexpected arg {0:?}")]
    UnexpectedArg(Option<MboxReply>),
}

/// Lists the mbox files of a store.
#[derive(Debug)]
pub struct MboxList {
    store: MboxStore,
    dirs: VecDeque<MboxFsPath>,
    mboxes: BTreeSet<Mbox>,
    state: State,
}

impl MboxList {
    /// Builds a coroutine listing the mailboxes of `store`.
    pub fn new(store: MboxStore) -> Self {
        let dirs = VecDeque::from([store.root.clone()]);
        Self {
            store,
            dirs,
            mboxes: BTreeSet::new(),
            state: State::Start,
        }
    }

    fn next_dir(&mut self) -> MboxCoroutineState<MboxYield, <Self as MboxCoroutine>::Return> {
        if let Some(dir) = self.dirs.pop_front() {
            self.state = State::ReadDir;
            return MboxCoroutineState::Yielded(MboxYield::WantsDirRead(dir));
        }

        if let Some(inbox) = self.store.inbox.clone() {
            self.state = State::StatInbox(inbox.clone());
            return MboxCoroutineState::Yielded(MboxYield::WantsFileMeta(inbox));
        }

        self.done()
    }

    fn done(&mut self) -> MboxCoroutineState<MboxYield, <Self as MboxCoroutine>::Return> {
        debug!("listed mboxes");
        trace!("count: {}", self.mboxes.len());
        self.state = State::Done;
        MboxCoroutineState::Complete(Ok(mem::take(&mut self.mboxes)))
    }
}

impl MboxCoroutine for MboxList {
    type Yield = MboxYield;
    type Return = Result<BTreeSet<Mbox>, MboxListError>;

    fn resume(&mut self, arg: Option<MboxReply>) -> MboxCoroutineState<Self::Yield, Self::Return> {
        match (&self.state, arg) {
            (State::Start, None) => self.next_dir(),
            (State::ReadDir, Some(MboxReply::DirRead(entries))) => {
                let mut dirs = Vec::new();
                for (path, kind) in entries {
                    let Some(name) = path.file_name() else {
                        continue;
                    };
                    if name.starts_with('.') {
                        continue;
                    }
                    match kind {
                        MboxFileKind::Dir => dirs.push(path),
                        MboxFileKind::File => {
                            if IGNORED_SUFFIXES.iter().any(|suffix| name.ends_with(suffix)) {
                                continue;
                            }
                            if self.store.inbox.as_ref() == Some(&path) {
                                continue;
                            }
                            if let Some(name) = self.store.relative(&path) {
                                self.mboxes.insert(Mbox { name, path });
                            }
                        }
                        MboxFileKind::Other => {}
                    }
                }
                self.dirs.extend(dirs);
                self.next_dir()
            }
            (State::StatInbox(inbox), Some(MboxReply::FileMeta(meta))) => {
                if meta.is_some() {
                    let mbox = Mbox {
                        name: MboxPath::from(INBOX),
                        path: inbox.clone(),
                    };
                    self.mboxes.insert(mbox);
                }
                self.done()
            }
            (_, arg) => MboxCoroutineState::Complete(Err(MboxListError::UnexpectedArg(arg))),
        }
    }
}

#[derive(Debug)]
enum State {
    Start,
    ReadDir,
    StatInbox(MboxFsPath),
    Done,
}

impl fmt::Display for State {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Start => f.write_str("start"),
            Self::ReadDir => f.write_str("read dir"),
            Self::StatInbox(_) => f.write_str("stat inbox"),
            Self::Done => f.write_str("done"),
        }
    }
}

#[cfg(test)]
mod tests {
    use alloc::{collections::BTreeMap, vec::Vec};

    use crate::{coroutine::*, mbox::list::*, path::MboxFsPath, store::MboxStore};

    fn dir(entries: &[(&str, MboxFileKind)]) -> MboxReply {
        let map: BTreeMap<_, _> = entries
            .iter()
            .map(|(p, k)| (MboxFsPath::new(*p), *k))
            .collect();
        MboxReply::DirRead(map)
    }

    #[test]
    fn walks_the_tree() {
        let store = MboxStore {
            root: MboxFsPath::new("/m"),
            inbox: Some(MboxFsPath::new("/var/mail/me")),
            thunderbird: true,
        };
        let mut cor = MboxList::new(store);
        assert!(matches!(
            cor.resume(None),
            MboxCoroutineState::Yielded(MboxYield::WantsDirRead(_))
        ));

        let root = dir(&[
            ("/m/a", MboxFileKind::File),
            ("/m/a.msf", MboxFileKind::File),
            ("/m/a.sbd", MboxFileKind::Dir),
            ("/m/.hidden", MboxFileKind::File),
            ("/m/b.lock", MboxFileKind::File),
            ("/m/sock", MboxFileKind::Other),
        ]);
        assert!(matches!(
            cor.resume(Some(root)),
            MboxCoroutineState::Yielded(MboxYield::WantsDirRead(p)) if p.as_str() == "/m/a.sbd"
        ));
        let sub = dir(&[("/m/a.sbd/c", MboxFileKind::File)]);
        assert!(matches!(
            cor.resume(Some(sub)),
            MboxCoroutineState::Yielded(MboxYield::WantsFileMeta(_))
        ));
        let MboxCoroutineState::Complete(Ok(mboxes)) =
            cor.resume(Some(MboxReply::FileMeta(Some(Default::default()))))
        else {
            panic!("expected mboxes");
        };
        let names: Vec<&str> = mboxes.iter().map(|m| m.name.as_str()).collect();
        assert_eq!(names, ["INBOX", "a", "a/c"]);
    }

    #[test]
    fn missing_inbox_and_bad_replies() {
        let store = MboxStore {
            root: MboxFsPath::new("/m"),
            inbox: Some(MboxFsPath::new("/m/in")),
            thunderbird: false,
        };
        let mut cor = MboxList::new(store);
        cor.resume(None);
        cor.resume(Some(dir(&[("/m/in", MboxFileKind::File)])));
        let MboxCoroutineState::Complete(Ok(mboxes)) = cor.resume(Some(MboxReply::FileMeta(None)))
        else {
            panic!("expected mboxes");
        };
        assert!(mboxes.is_empty());

        let mut cor = MboxList::new(MboxStore::default());
        assert!(matches!(
            cor.resume(Some(MboxReply::Sleep)),
            MboxCoroutineState::Complete(Err(MboxListError::UnexpectedArg(_)))
        ));
        cor = MboxList::new(MboxStore::default());
        cor.resume(None);
        assert!(matches!(
            cor.resume(Some(dir(&[]))),
            MboxCoroutineState::Complete(Ok(_))
        ));
    }
}
