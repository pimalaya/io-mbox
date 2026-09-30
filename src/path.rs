//! # Paths
//!
//! Two path flavours used across the crate: literal filesystem paths
//! ([`MboxFsPath`]) and logical mailbox names ([`MboxPath`]). A
//! [`crate::store::MboxStore`] translates between them under its
//! configured layout.

use core::fmt;

use alloc::string::String;

/// Forward-slash separated literal filesystem path.
///
/// Always uses `/` regardless of host OS: `std::fs` accepts `/`-paths on
/// both Unix and Windows, so no conversion is needed in the client layer.
#[derive(Clone, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct MboxFsPath(String);

impl MboxFsPath {
    /// Builds a new path from `s` without validation.
    pub fn new(s: impl Into<String>) -> Self {
        Self(s.into())
    }

    /// Returns the path as a `&str`.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Returns `true` when the path is empty.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Returns a new path with `segment` appended after a `/` separator.
    ///
    /// If `self` is empty the result is `segment` alone, and a trailing
    /// `/` in `self` is not doubled.
    pub fn join(&self, segment: &str) -> Self {
        let mut out = self.0.clone();
        if !out.is_empty() && !out.ends_with('/') {
            out.push('/');
        }
        out.push_str(segment);
        Self(out)
    }

    /// Returns a new path with `suffix` appended to the final component.
    pub fn with_suffix(&self, suffix: &str) -> Self {
        let mut out = self.0.clone();
        out.push_str(suffix);
        Self(out)
    }

    /// Returns the final path component, if any.
    pub fn file_name(&self) -> Option<&str> {
        match self.0.rsplit_once('/') {
            Some((_, name)) if !name.is_empty() => Some(name),
            None if !self.0.is_empty() => Some(&self.0),
            _ => None,
        }
    }

    /// Returns the path without its final component, if any.
    pub fn parent(&self) -> Option<Self> {
        match self.0.rsplit_once('/') {
            Some(("", _)) => Some(Self::new("/")),
            Some((parent, _)) => Some(Self::new(parent)),
            None => None,
        }
    }

    /// If `self` is rooted at `prefix`, returns the relative remainder
    /// without its leading `/`.
    pub fn strip_prefix(&self, prefix: &Self) -> Option<&str> {
        let rest = self.0.strip_prefix(prefix.as_str())?;
        if rest.is_empty() {
            return Some(rest);
        }
        if prefix.0.ends_with('/') {
            return Some(rest);
        }
        rest.strip_prefix('/')
    }
}

impl fmt::Display for MboxFsPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.0, f)
    }
}

impl From<String> for MboxFsPath {
    fn from(s: String) -> Self {
        Self(s)
    }
}

impl From<&str> for MboxFsPath {
    fn from(s: &str) -> Self {
        Self(s.into())
    }
}

impl AsRef<str> for MboxFsPath {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

#[cfg(feature = "client")]
impl From<std::path::PathBuf> for MboxFsPath {
    fn from(path: std::path::PathBuf) -> Self {
        Self::from(path.as_path())
    }
}

#[cfg(feature = "client")]
impl From<&std::path::Path> for MboxFsPath {
    fn from(path: &std::path::Path) -> Self {
        let s = path.to_string_lossy().into_owned();
        #[cfg(windows)]
        let s = s.replace('\\', "/");
        Self(s)
    }
}

#[cfg(feature = "client")]
impl AsRef<std::path::Path> for MboxFsPath {
    fn as_ref(&self) -> &std::path::Path {
        std::path::Path::new(&self.0)
    }
}

/// Logical mailbox name, always `/`-separated whatever the on-disk
/// layout.
#[derive(Clone, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct MboxPath(String);

impl MboxPath {
    /// Returns the name as a `&str`.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Returns `true` when the name is empty.
    pub fn is_empty(&self) -> bool {
        self.components().next().is_none()
    }

    /// Iterates over the non-empty hierarchy segments.
    pub fn components(&self) -> impl Iterator<Item = &str> {
        self.0.split('/').filter(|c| !c.is_empty())
    }
}

impl fmt::Display for MboxPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.0, f)
    }
}

impl From<String> for MboxPath {
    fn from(s: String) -> Self {
        Self(s)
    }
}

impl From<&str> for MboxPath {
    fn from(s: &str) -> Self {
        Self(s.into())
    }
}

impl AsRef<str> for MboxPath {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

#[cfg(test)]
mod tests {
    use alloc::vec::Vec;

    use crate::path::{MboxFsPath, MboxPath};

    #[test]
    fn join_inserts_one_separator() {
        assert_eq!(MboxFsPath::new("a").join("b").as_str(), "a/b");
        assert_eq!(MboxFsPath::new("a/").join("b").as_str(), "a/b");
        assert_eq!(MboxFsPath::default().join("b").as_str(), "b");
    }

    #[test]
    fn file_name_and_parent() {
        let path = MboxFsPath::new("/var/mail/root");
        assert_eq!(path.file_name(), Some("root"));
        assert_eq!(path.parent(), Some(MboxFsPath::new("/var/mail")));
        assert_eq!(
            MboxFsPath::new("/root").parent(),
            Some(MboxFsPath::new("/"))
        );
        assert_eq!(MboxFsPath::new("root").parent(), None);
        assert_eq!(MboxFsPath::new("a/").file_name(), None);
        assert_eq!(MboxFsPath::new("a").file_name(), Some("a"));
    }

    #[test]
    fn with_suffix_appends_to_the_final_component() {
        let path = MboxFsPath::new("/var/mail/root").with_suffix(".lock");
        assert_eq!(path.as_str(), "/var/mail/root.lock");
    }

    #[test]
    fn strip_prefix_drops_the_separator() {
        let root = MboxFsPath::new("/mail");
        assert_eq!(
            MboxFsPath::new("/mail/a/b").strip_prefix(&root),
            Some("a/b")
        );
        assert_eq!(MboxFsPath::new("/mail").strip_prefix(&root), Some(""));
        assert_eq!(MboxFsPath::new("/mailx").strip_prefix(&root), None);
        let root = MboxFsPath::new("/mail/");
        assert_eq!(MboxFsPath::new("/mail/a").strip_prefix(&root), Some("a"));
    }

    #[test]
    fn mbox_path_components_skip_empties() {
        let path = MboxPath::from("/lists//rust/");
        let parts: Vec<&str> = path.components().collect();
        assert_eq!(parts, ["lists", "rust"]);
        assert!(MboxPath::from("//").is_empty());
    }
}
