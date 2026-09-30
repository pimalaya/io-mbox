//! # Flags
//!
//! Message flags and the headers carrying them inside an mbox.
//!
//! mbox has no out-of-band metadata, so flags live in the message
//! headers. [`MboxFlags`] reads and writes the c-client convention shared
//! by UW-IMAP, Dovecot, mutt and meli: `Status` holds `R` (seen) and `O`
//! (old, no longer recent), `X-Status` holds `A` (answered), `F`
//! (flagged), `T` (draft) and `D` (deleted), and `X-Keywords` holds the
//! custom keywords. A message carrying none of those falls back to
//! Thunderbird's `X-Mozilla-Status` bitfield and `X-Mozilla-Keys`, which
//! are read but never written.
//!
//! [`crate::scan`] records where the three written fields sit in each
//! message, so [`crate::entry::update`] can swap them without parsing the
//! message again.
//!
//! Refs: <https://doc.dovecot.org/admin_manual/mailbox_formats/mbox/>

use core::fmt;

use alloc::{collections::BTreeSet, string::String, vec::Vec};

use crate::header;

/// Header fields [`MboxFlags::to_header`] writes, lowercase.
pub(crate) const FLAG_FIELDS: [&str; 3] = ["status", "x-status", "x-keywords"];

/// One flag of an mbox message.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum MboxFlag {
    /// `Status: R`, the message was read.
    Seen,
    /// `Status: O`, a client saw the message arrive, so it is no longer
    /// recent.
    Old,
    /// `X-Status: A`, the message was replied to.
    Answered,
    /// `X-Status: F`, the message is flagged for attention.
    Flagged,
    /// `X-Status: T`, the message is a draft.
    Draft,
    /// `X-Status: D`, the message is marked for deletion.
    Deleted,
    /// A custom keyword from `X-Keywords`.
    Keyword(String),
}

/// The flag set of an mbox message.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct MboxFlags(pub BTreeSet<MboxFlag>);

impl MboxFlags {
    /// Returns `true` when `flag` is set.
    pub fn contains(&self, flag: &MboxFlag) -> bool {
        self.0.contains(flag)
    }

    /// Sets `flag`, returning `true` when it was not set.
    pub fn insert(&mut self, flag: MboxFlag) -> bool {
        self.0.insert(flag)
    }

    /// Clears `flag`, returning `true` when it was set.
    pub fn remove(&mut self, flag: &MboxFlag) -> bool {
        self.0.remove(flag)
    }

    /// Returns `true` when no flag is set.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Iterates over the flags, sorted.
    pub fn iter(&self) -> impl Iterator<Item = &MboxFlag> {
        self.0.iter()
    }

    /// Reads the flags carried by a header block.
    ///
    /// The c-client fields win when any of them is present. Otherwise the
    /// Thunderbird fields are read, so a mailbox Thunderbird wrote shows
    /// its read and flagged states.
    pub fn from_header(block: &[u8]) -> Self {
        let mut flags = Self::default();
        let mut c_client = false;
        let mut mozilla = None;
        let mut mozilla_keys = None;

        for (name, value) in header::fields(block) {
            let name = String::from_utf8_lossy(name).to_ascii_lowercase();
            match name.as_str() {
                "status" => {
                    c_client = true;
                    for b in value {
                        match b {
                            b'R' => flags.insert(MboxFlag::Seen),
                            b'O' => flags.insert(MboxFlag::Old),
                            _ => false,
                        };
                    }
                }
                "x-status" => {
                    c_client = true;
                    for b in value {
                        match b {
                            b'A' => flags.insert(MboxFlag::Answered),
                            b'F' => flags.insert(MboxFlag::Flagged),
                            b'T' => flags.insert(MboxFlag::Draft),
                            b'D' => flags.insert(MboxFlag::Deleted),
                            _ => false,
                        };
                    }
                }
                "x-keywords" => {
                    c_client = true;
                    flags.insert_keywords(&value);
                }
                "x-mozilla-status" => {
                    let hex = String::from_utf8_lossy(&value);
                    mozilla = u16::from_str_radix(hex.trim(), 16).ok();
                }
                "x-mozilla-keys" => mozilla_keys = Some(value),
                _ => {}
            }
        }

        if c_client {
            return flags;
        }

        if let Some(bits) = mozilla {
            for (bit, flag) in [
                (0x0001, MboxFlag::Seen),
                (0x0002, MboxFlag::Answered),
                (0x0004, MboxFlag::Flagged),
                (0x0008, MboxFlag::Deleted),
            ] {
                if bits & bit != 0 {
                    flags.insert(flag);
                }
            }
        }

        if let Some(keys) = mozilla_keys {
            flags.insert_keywords(&keys);
        }

        flags
    }

    /// Renders the flag fields to insert into a header block, each line
    /// ended with CRLF when `crlf` is set. An empty set renders nothing.
    pub fn to_header(&self, crlf: bool) -> Vec<u8> {
        let eol: &[u8] = if crlf { b"\r\n" } else { b"\n" };
        let mut out = Vec::new();

        let mut status = String::new();
        let mut x_status = String::new();
        let mut keywords = Vec::new();
        for flag in self.iter() {
            match flag {
                MboxFlag::Seen => status.push('R'),
                MboxFlag::Old => status.push('O'),
                MboxFlag::Answered => x_status.push('A'),
                MboxFlag::Flagged => x_status.push('F'),
                MboxFlag::Draft => x_status.push('T'),
                MboxFlag::Deleted => x_status.push('D'),
                MboxFlag::Keyword(keyword) => keywords.push(keyword.as_str()),
            }
        }

        for (name, value) in [
            ("Status", status),
            ("X-Status", x_status),
            ("X-Keywords", keywords.join(" ")),
        ] {
            if !value.is_empty() {
                out.extend_from_slice(name.as_bytes());
                out.extend_from_slice(b": ");
                out.extend_from_slice(value.as_bytes());
                out.extend_from_slice(eol);
            }
        }

        out
    }

    fn insert_keywords(&mut self, value: &[u8]) {
        let value = String::from_utf8_lossy(value);
        for keyword in value.split(|c: char| c == ',' || c.is_ascii_whitespace()) {
            if !keyword.is_empty() {
                self.insert(MboxFlag::Keyword(keyword.into()));
            }
        }
    }
}

impl FromIterator<MboxFlag> for MboxFlags {
    fn from_iter<T: IntoIterator<Item = MboxFlag>>(iter: T) -> Self {
        Self(iter.into_iter().collect())
    }
}

impl Extend<MboxFlag> for MboxFlags {
    fn extend<T: IntoIterator<Item = MboxFlag>>(&mut self, iter: T) {
        self.0.extend(iter)
    }
}

impl fmt::Display for MboxFlag {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Seen => f.write_str("seen"),
            Self::Old => f.write_str("old"),
            Self::Answered => f.write_str("answered"),
            Self::Flagged => f.write_str("flagged"),
            Self::Draft => f.write_str("draft"),
            Self::Deleted => f.write_str("deleted"),
            Self::Keyword(keyword) => f.write_str(keyword),
        }
    }
}

#[cfg(test)]
mod tests {
    use alloc::string::ToString;

    use crate::flag::{MboxFlag, MboxFlags};

    fn flags(list: &[MboxFlag]) -> MboxFlags {
        list.iter().cloned().collect()
    }

    #[test]
    fn c_client_fields_parse() {
        let block = b"Subject: x\nStatus: RO\nX-Status: AFTD\nX-Keywords: $Label1, work\n  junk\n";
        let expected = flags(&[
            MboxFlag::Seen,
            MboxFlag::Old,
            MboxFlag::Answered,
            MboxFlag::Flagged,
            MboxFlag::Draft,
            MboxFlag::Deleted,
            MboxFlag::Keyword("$Label1".into()),
            MboxFlag::Keyword("work".into()),
            MboxFlag::Keyword("junk".into()),
        ]);
        assert_eq!(MboxFlags::from_header(block), expected);
    }

    #[test]
    fn mozilla_fields_are_a_fallback() {
        let block = b"X-Mozilla-Status: 0005\nX-Mozilla-Keys: todo\n";
        let expected = flags(&[
            MboxFlag::Seen,
            MboxFlag::Flagged,
            MboxFlag::Keyword("todo".into()),
        ]);
        assert_eq!(MboxFlags::from_header(block), expected);

        let block = b"X-Mozilla-Status: 000a\nStatus: O\n";
        assert_eq!(MboxFlags::from_header(block), flags(&[MboxFlag::Old]));

        let block = b"X-Mozilla-Status: zz\n";
        assert!(MboxFlags::from_header(block).is_empty());

        let block = b"Status: RXO\nX-Status: ZF\n";
        let expected = flags(&[MboxFlag::Seen, MboxFlag::Old, MboxFlag::Flagged]);
        assert_eq!(MboxFlags::from_header(block), expected);
    }

    #[test]
    fn header_round_trips() {
        let all = flags(&[
            MboxFlag::Seen,
            MboxFlag::Old,
            MboxFlag::Answered,
            MboxFlag::Flagged,
            MboxFlag::Draft,
            MboxFlag::Deleted,
            MboxFlag::Keyword("a".into()),
            MboxFlag::Keyword("b".into()),
        ]);
        let header = all.to_header(false);
        assert_eq!(
            header,
            b"Status: RO\nX-Status: AFTD\nX-Keywords: a b\n".as_slice()
        );
        assert_eq!(MboxFlags::from_header(&header), all);

        let header = flags(&[MboxFlag::Flagged]).to_header(true);
        assert_eq!(header, b"X-Status: F\r\n".as_slice());
        assert!(MboxFlags::default().to_header(false).is_empty());
    }

    #[test]
    fn set_operations() {
        let mut set = MboxFlags::default();
        assert!(set.insert(MboxFlag::Seen));
        assert!(!set.insert(MboxFlag::Seen));
        assert!(set.contains(&MboxFlag::Seen));
        set.extend([MboxFlag::Draft]);
        assert_eq!(set.iter().count(), 2);
        assert!(set.remove(&MboxFlag::Seen));
        assert!(!set.remove(&MboxFlag::Seen));
        assert_eq!(MboxFlag::Keyword("k".into()).to_string(), "k");
        assert_eq!(MboxFlag::Deleted.to_string(), "deleted");
        for (flag, name) in [
            (MboxFlag::Seen, "seen"),
            (MboxFlag::Old, "old"),
            (MboxFlag::Answered, "answered"),
            (MboxFlag::Flagged, "flagged"),
            (MboxFlag::Draft, "draft"),
        ] {
            assert_eq!(flag.to_string(), name);
        }
    }
}
