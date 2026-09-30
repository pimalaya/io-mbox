//! # Entry
//!
//! One message of an mbox: [`MboxEntry`] locates it in the file and
//! carries what a listing needs, [`MboxFullEntry`] adds its bytes.
//!
//! Entries come from [`crate::scan::MboxScanner`], usually through
//! [`crate::index::sync`]. The coroutines next to this file act on them:
//! [`get`] reads one, [`append`] adds messages, [`update`] rewrites flags
//! and removes messages, [`copy`] and [`move`] combine the two across
//! files.

pub mod append;
pub mod copy;
pub mod get;
pub mod r#move;
pub mod update;

use alloc::{string::String, vec::Vec};

use crate::flag::MboxFlags;

/// A message located in an mbox file.
///
/// The layout is `From_ line | header | blank line | body | separator`,
/// where the separator is the blank line preceding the next `From_` line.
/// `offset` points at the `From_` line, `message_offset` right after it.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct MboxEntry {
    /// Content id: a hash of the message with its volatile header fields
    /// left out, stable across flag writes and rewrites. Byte-identical
    /// duplicates get a `-2`, `-3` suffix in file order.
    pub id: String,
    /// Offset of the `From_` line.
    pub offset: u64,
    /// Offset of the message, right after the `From_` line.
    pub message_offset: u64,
    /// Length of the message, separator excluded, still quoted.
    pub len: u64,
    /// Length of the header, blank line excluded. Equals `len` when the
    /// message has no blank line.
    pub header_len: u64,
    /// Flags read from the header.
    pub flags: MboxFlags,
    /// Whether the `From_` line ends with CRLF, which new header lines
    /// follow.
    pub crlf: bool,
    /// Where the flag fields sit, relative to `message_offset`, so a flag
    /// write can drop them without parsing the message again.
    pub flag_spans: Vec<MboxSpan>,
    /// Envelope sender from the `From_` line.
    pub sender: String,
    /// Delivery date from the `From_` line, seconds since the Unix epoch.
    pub timestamp: i64,
    /// The raw header, when the scan was asked to keep it, capped.
    pub header: Vec<u8>,
    /// Whether the message is a c-client `X-IMAP` pseudo message holding
    /// folder metadata, which clients hide.
    pub pseudo: bool,
}

impl MboxEntry {
    /// Offset right after the message, where its separator starts.
    pub fn end(&self) -> u64 {
        self.message_offset + self.len
    }
}

/// A byte range, relative to [`MboxEntry::message_offset`].
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct MboxSpan {
    /// Start of the range.
    pub offset: u64,
    /// Length of the range.
    pub len: u64,
}

/// A message with its bytes, unquoted.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct MboxFullEntry {
    /// Where the message sits and what the scan saw.
    pub entry: MboxEntry,
    /// The message bytes, unquoted, `From_` line and separator excluded.
    pub contents: Vec<u8>,
}

impl MboxFullEntry {
    /// Parses the message with mail-parser.
    #[cfg(feature = "parser")]
    pub fn parsed(&self) -> Option<mail_parser::Message<'_>> {
        mail_parser::MessageParser::new().parse(&self.contents)
    }
}
