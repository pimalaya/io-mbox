//! # Store
//!
//! Layout-aware view of a tree of mbox files. Wraps the filesystem root,
//! the optional spool standing for `INBOX`, and the layout convention
//! mapping a logical [`MboxPath`] to its on-disk [`MboxFsPath`].
//! [`crate::mbox::list`] walks the same layout the other way round.

use alloc::string::String;

use crate::path::{MboxFsPath, MboxPath};

/// Logical name of the spool mailbox, when [`MboxStore::inbox`] is set.
pub const INBOX: &str = "INBOX";

/// Suffix of the directory holding the children of a Thunderbird mbox.
pub const SBD: &str = ".sbd";

/// Directory holding mbox files, plus the spool and the layout used to
/// translate logical mailbox names to on-disk paths.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct MboxStore {
    /// Directory holding the mbox files, walked recursively.
    pub root: MboxFsPath,
    /// Spool file surfaced as [`INBOX`] (typically `$MAIL`, for example
    /// /var/mail/user), wherever it lives.
    pub inbox: Option<MboxFsPath>,
    /// Thunderbird layout: the children of mailbox `a` live in the
    /// directory `a.sbd`. Default off: children live in the plain
    /// directory `a`, as mutt and most MUAs lay them out.
    pub thunderbird: bool,
}

impl MboxStore {
    /// Resolves a logical mailbox name to its on-disk path.
    ///
    /// `INBOX` (any case) resolves to [`Self::inbox`] when set. Otherwise
    /// `a/b` resolves to `<root>/a/b`, or `<root>/a.sbd/b` in the
    /// Thunderbird layout.
    pub fn resolve(&self, name: &MboxPath) -> MboxFsPath {
        if let Some(inbox) = &self.inbox
            && name.as_str().eq_ignore_ascii_case(INBOX)
        {
            return inbox.clone();
        }

        let mut rel = String::new();
        let mut components = name.components().peekable();
        while let Some(component) = components.next() {
            if !rel.is_empty() {
                rel.push('/');
            }
            rel.push_str(component);
            if self.thunderbird && components.peek().is_some() {
                rel.push_str(SBD);
            }
        }

        self.root.join(&rel)
    }

    /// Reverse of [`Self::resolve`]: the logical name of an on-disk path,
    /// or `None` when it lies outside the store.
    pub fn relative(&self, path: &MboxFsPath) -> Option<MboxPath> {
        if self.inbox.as_ref() == Some(path) {
            return Some(MboxPath::from(INBOX));
        }

        let rel = path.strip_prefix(&self.root)?;
        if rel.is_empty() {
            return None;
        }

        let mut name = String::with_capacity(rel.len());
        let mut components = rel.split('/').filter(|c| !c.is_empty()).peekable();
        while let Some(component) = components.next() {
            if !name.is_empty() {
                name.push('/');
            }
            let component = match components.peek() {
                Some(_) if self.thunderbird => component.strip_suffix(SBD).unwrap_or(component),
                _ => component,
            };
            name.push_str(component);
        }

        Some(MboxPath::from(name))
    }
}

#[cfg(test)]
mod tests {
    use crate::{
        path::{MboxFsPath, MboxPath},
        store::MboxStore,
    };

    fn store(thunderbird: bool) -> MboxStore {
        MboxStore {
            root: MboxFsPath::new("/mail"),
            inbox: Some(MboxFsPath::new("/var/mail/me")),
            thunderbird,
        }
    }

    #[test]
    fn inbox_resolves_to_the_spool() {
        let store = store(false);
        let spool = MboxFsPath::new("/var/mail/me");
        assert_eq!(store.resolve(&MboxPath::from("inbox")), spool);
        assert_eq!(store.relative(&spool), Some(MboxPath::from("INBOX")));
    }

    #[test]
    fn inbox_without_spool_is_a_plain_file() {
        let store = MboxStore {
            inbox: None,
            ..store(false)
        };
        let path = store.resolve(&MboxPath::from("INBOX"));
        assert_eq!(path.as_str(), "/mail/INBOX");
    }

    #[test]
    fn plain_layout_round_trips() {
        let store = store(false);
        let path = store.resolve(&MboxPath::from("lists/rust"));
        assert_eq!(path.as_str(), "/mail/lists/rust");
        assert_eq!(store.relative(&path), Some(MboxPath::from("lists/rust")));
    }

    #[test]
    fn thunderbird_layout_round_trips() {
        let store = store(true);
        let path = store.resolve(&MboxPath::from("a/b/c"));
        assert_eq!(path.as_str(), "/mail/a.sbd/b.sbd/c");
        assert_eq!(store.relative(&path), Some(MboxPath::from("a/b/c")));
    }

    #[test]
    fn outside_paths_have_no_name() {
        let store = store(false);
        assert_eq!(store.relative(&MboxFsPath::new("/elsewhere/x")), None);
        assert_eq!(store.relative(&MboxFsPath::new("/mail")), None);
    }
}
