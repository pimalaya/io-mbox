//! # Index
//!
//! [`MboxIndex`], the entries of an mbox file stamped with what the file
//! looked like when they were scanned, so the next reader skips the
//! scan when nothing changed.
//!
//! An mbox has no directory of its contents: finding a message means
//! reading the file up to it. The index keeps the result of that read,
//! and [`sync`] brings it up to date: an unchanged file reuses it, a file
//! that only grew (a delivery appends) is scanned from its last entry
//! on, and anything else is scanned again. The crate never stores an
//! index itself. A caller persists it wherever suits it, through serde
//! with the `serde` feature.

pub mod sync;

use alloc::{string::String, vec::Vec};

use sha2::{Digest, Sha256};

use crate::{coroutine::MboxFileMeta, entry::MboxEntry, scan::MboxScannerOptions};

/// How many bytes at the end of the file [`MboxIndex::tail`] covers.
pub(crate) const TAIL_LEN: u64 = 4096;

/// The entries of an mbox file and the file state they describe.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct MboxIndex {
    /// The file stat at scan time.
    pub meta: MboxFileMeta,
    /// Hash of the last 4 KiB at scan time, which must still
    /// be there for an append-only growth to be resumed.
    pub tail: String,
    /// The scanner options the entries were produced with.
    pub opts: MboxScannerOptions,
    /// The entries, in file order, pseudo message included.
    pub entries: Vec<MboxEntry>,
}

impl MboxIndex {
    /// Returns the entry with the given id.
    pub fn get(&self, id: &str) -> Option<&MboxEntry> {
        self.entries.iter().find(|entry| entry.id == id)
    }

    /// Iterates over the entries a client shows, pseudo message left out.
    pub fn messages(&self) -> impl Iterator<Item = &MboxEntry> {
        self.entries.iter().filter(|entry| !entry.pseudo)
    }

    /// Offset where the entry at `i` ends, separator included: the next
    /// entry's offset, or the file size for the last one.
    pub fn next_offset(&self, i: usize) -> u64 {
        match self.entries.get(i + 1) {
            Some(next) => next.offset,
            None => self.meta.size,
        }
    }
}

/// Hash stored in [`MboxIndex::tail`].
pub(crate) fn tail_hash(bytes: &[u8]) -> String {
    let hash = Sha256::digest(bytes);
    let mut out = String::with_capacity(32);
    for byte in &hash[..16] {
        out.push(char::from_digit((byte >> 4) as u32, 16).unwrap_or('0'));
        out.push(char::from_digit((byte & 15) as u32, 16).unwrap_or('0'));
    }
    out
}

/// Start and length of the tail of a file of `size` bytes.
pub(crate) fn tail_range(size: u64) -> (u64, usize) {
    let len = size.min(TAIL_LEN);
    (size - len, len as usize)
}

#[cfg(test)]
mod tests {
    use crate::{
        coroutine::MboxFileMeta,
        entry::MboxEntry,
        index::{MboxIndex, tail_hash, tail_range},
    };

    #[test]
    fn lookups() {
        let index = MboxIndex {
            entries: vec![
                MboxEntry {
                    id: "a".into(),
                    pseudo: true,
                    ..Default::default()
                },
                MboxEntry {
                    id: "b".into(),
                    offset: 10,
                    ..Default::default()
                },
            ],
            meta: MboxFileMeta {
                size: 20,
                ..Default::default()
            },
            ..Default::default()
        };
        assert_eq!(index.get("b").map(|e| e.offset), Some(10));
        assert!(index.get("c").is_none());
        assert_eq!(index.messages().count(), 1);
        assert_eq!(index.next_offset(0), 10);
        assert_eq!(index.next_offset(1), 20);
    }

    #[test]
    fn tail_helpers() {
        assert_eq!(tail_range(10), (0, 10));
        assert_eq!(tail_range(10_000), (10_000 - 4096, 4096));
        assert_eq!(tail_hash(b"").len(), 32);
        assert_ne!(tail_hash(b"a"), tail_hash(b"b"));
    }
}
