//! I/O-free coroutine listing the mbox files of a store.
//!
//! The root is walked recursively. Every regular file is a mailbox, except
//! hidden files and the side files of other tools (`.lock` dotlocks,
//! Thunderbird `.msf` summaries). A directory named like a dotlock or a
//! summary is not walked, no name resolving into it. The spool, when set,
//! is listed as `INBOX` if it exists.
//!
//! Each directory is walked once, whatever symbolic links lead to it: the
//! walk tracks the canonical path of every directory it reaches, and a
//! link to a directory is followed last, only when it leads somewhere
//! new. A directory below the root is so listed under its own name, and a
//! link back into the tree cannot make the walk loop.

use core::{fmt, mem};

use alloc::collections::{BTreeSet, VecDeque};

use log::{debug, trace};
use thiserror::Error;

use crate::{
    coroutine::*,
    mbox::Mbox,
    path::{MboxFsPath, MboxPath},
    store::{INBOX, LOCK, MSF, MboxStore},
};

/// Suffix of a file another tool keeps beside an mbox, no mailbox.
const LCK: &str = ".lck";

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
    /// Directories left to read, each with its canonical path.
    dirs: VecDeque<(MboxFsPath, MboxFsPath)>,
    /// Links to a directory, resolved once no directory is left to read.
    links: VecDeque<MboxFsPath>,
    /// Canonical paths of the directories reached.
    reached: BTreeSet<MboxFsPath>,
    mboxes: BTreeSet<Mbox>,
    state: State,
}

impl MboxList {
    /// Builds a coroutine listing the mailboxes of `store`.
    pub fn new(store: MboxStore) -> Self {
        Self {
            store,
            dirs: VecDeque::new(),
            links: VecDeque::new(),
            reached: BTreeSet::new(),
            mboxes: BTreeSet::new(),
            state: State::Start,
        }
    }

    /// Queues the directory at `path` unless `canonical` was reached.
    fn reach(&mut self, path: MboxFsPath, canonical: MboxFsPath) {
        if self.reached.insert(canonical.clone()) {
            self.dirs.push_back((path, canonical));
        }
    }

    fn next_dir(&mut self) -> MboxCoroutineState<MboxYield, <Self as MboxCoroutine>::Return> {
        if let Some((dir, canonical)) = self.dirs.pop_front() {
            self.state = State::ReadDir(canonical);
            return MboxCoroutineState::Yielded(MboxYield::WantsDirRead(dir));
        }

        if let Some(link) = self.links.pop_front() {
            self.state = State::ResolveLink(link.clone());
            return MboxCoroutineState::Yielded(MboxYield::WantsDirCanonical(link));
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
        match (&mut self.state, arg) {
            (State::Start, None) => {
                self.state = State::ResolveRoot;
                let root = self.store.root.clone();
                MboxCoroutineState::Yielded(MboxYield::WantsDirCanonical(root))
            }
            (State::ResolveRoot, Some(MboxReply::DirCanonical(canonical))) => {
                let root = self.store.root.clone();
                let canonical = canonical.unwrap_or_else(|| root.clone());
                self.reach(root, canonical);
                self.next_dir()
            }
            (State::ResolveLink(link), Some(MboxReply::DirCanonical(canonical))) => {
                let link = mem::take(link);
                if let Some(canonical) = canonical {
                    self.reach(link, canonical);
                }
                self.next_dir()
            }
            (State::ReadDir(canonical), Some(MboxReply::DirRead(entries))) => {
                let canonical = mem::take(canonical);
                for (path, kind) in entries {
                    let Some(name) = path.file_name() else {
                        continue;
                    };
                    let side = name.ends_with(LOCK) || name.ends_with(MSF);
                    if name.starts_with('.') || side {
                        continue;
                    }
                    match kind {
                        MboxFileKind::Dir => {
                            let canonical = canonical.join(name);
                            self.reach(path, canonical);
                        }
                        MboxFileKind::DirLink => self.links.push_back(path),
                        MboxFileKind::File => {
                            if name.ends_with(LCK) {
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
                self.next_dir()
            }
            (State::StatInbox(inbox), Some(MboxReply::FileMeta(meta))) => {
                if meta.is_some() {
                    let mbox = Mbox {
                        name: MboxPath::from(INBOX),
                        path: mem::take(inbox),
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
    ResolveRoot,
    /// Reading a directory, of which this is the canonical path.
    ReadDir(MboxFsPath),
    ResolveLink(MboxFsPath),
    StatInbox(MboxFsPath),
    Done,
}

impl fmt::Display for State {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Start => f.write_str("start"),
            Self::ResolveRoot => f.write_str("resolve root"),
            Self::ReadDir(_) => f.write_str("read dir"),
            Self::ResolveLink(_) => f.write_str("resolve link"),
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
            MboxCoroutineState::Yielded(MboxYield::WantsDirCanonical(p)) if p.as_str() == "/m"
        ));
        assert!(matches!(
            cor.resume(Some(MboxReply::DirCanonical(None))),
            MboxCoroutineState::Yielded(MboxYield::WantsDirRead(p)) if p.as_str() == "/m"
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
    fn walks_each_directory_once_whatever_links_lead_to_it() {
        let dirs = BTreeMap::from([
            (
                "/m",
                dir(&[
                    ("/m/a", MboxFileKind::File),
                    ("/m/back", MboxFileKind::DirLink),
                    ("/m/far", MboxFileKind::DirLink),
                    ("/m/gone", MboxFileKind::DirLink),
                    ("/m/sub", MboxFileKind::Dir),
                    ("/m/twin", MboxFileKind::DirLink),
                ]),
            ),
            (
                "/m/sub",
                dir(&[
                    ("/m/sub/b", MboxFileKind::File),
                    ("/m/sub/up", MboxFileKind::DirLink),
                ]),
            ),
            (
                "/m/far",
                dir(&[
                    ("/m/far/c", MboxFileKind::File),
                    ("/m/far/deep", MboxFileKind::Dir),
                    ("/m/far/self", MboxFileKind::DirLink),
                ]),
            ),
            ("/m/far/deep", dir(&[("/m/far/deep/d", MboxFileKind::File)])),
        ]);
        let canonical = BTreeMap::from([
            ("/m", "/real/m"),
            ("/m/back", "/real/m"),
            ("/m/far", "/far"),
            ("/m/twin", "/real/m/sub"),
            ("/m/sub/up", "/real/m"),
            ("/m/far/self", "/far"),
        ]);

        let store = MboxStore {
            root: MboxFsPath::new("/m"),
            ..Default::default()
        };
        let mut cor = MboxList::new(store);
        let mut read = Vec::new();
        let mut arg = None;
        let mboxes = loop {
            match cor.resume(arg.take()) {
                MboxCoroutineState::Complete(result) => break result.unwrap(),
                MboxCoroutineState::Yielded(MboxYield::WantsDirCanonical(path)) => {
                    let path = canonical.get(path.as_str()).copied();
                    arg = Some(MboxReply::DirCanonical(path.map(MboxFsPath::new)));
                }
                MboxCoroutineState::Yielded(MboxYield::WantsDirRead(path)) => {
                    arg = Some(dirs[path.as_str()].clone());
                    read.push(path);
                }
                state => panic!("unexpected {state:?}"),
            }
        };

        let read: Vec<&str> = read.iter().map(|path| path.as_str()).collect();
        assert_eq!(read, ["/m", "/m/sub", "/m/far", "/m/far/deep"]);
        let names: Vec<&str> = mboxes.iter().map(|m| m.name.as_str()).collect();
        assert_eq!(names, ["a", "far/c", "far/deep/d", "sub/b"]);
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
        cor.resume(Some(MboxReply::DirCanonical(None)));
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
        cor.resume(Some(MboxReply::DirCanonical(None)));
        assert!(matches!(
            cor.resume(Some(dir(&[]))),
            MboxCoroutineState::Complete(Ok(_))
        ));
    }
}
