//! # Mbox
//!
//! Mailbox lifecycle: one mailbox is one mbox file. [`Mbox`] pairs its
//! logical name with its path, and the coroutines next to this file
//! [`create`], [`delete`], [`list`] and [`rename`] whole mailboxes. Names
//! and paths translate through [`crate::store::MboxStore`].

pub mod create;
pub mod delete;
pub mod list;
pub mod rename;

use crate::path::{MboxFsPath, MboxPath};

/// An mbox file and its logical name.
#[derive(Clone, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct Mbox {
    /// Logical name, `/`-separated.
    pub name: MboxPath,
    /// Path of the file.
    pub path: MboxFsPath,
}
