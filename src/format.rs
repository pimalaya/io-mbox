//! # Format
//!
//! The four mbox variants and their `From ` quoting rules.
//!
//! Every variant separates messages with a `From_` line (see
//! [`crate::from_line`]). They differ in how a body line starting with
//! `From ` is protected. mboxo and mboxcl prefix it with one `>`, which
//! cannot be undone reliably since `>From ` itself is left alone. mboxrd
//! also quotes every already-quoted `>*From ` line, so unquoting is exact.
//! mboxcl and mboxcl2 add a `Content-Length` header, and mboxcl2 quotes
//! nothing, relying on that header alone.
//!
//! Refs: <https://web.archive.org/web/20160812091518/https://jdebp.eu./FGA/mail-mbox-formats.html>

use core::{fmt, str::FromStr};

use alloc::{string::String, vec::Vec};

use thiserror::Error;

/// Error returned when parsing an unknown [`MboxFormat`] name.
#[derive(Clone, Debug, Eq, Error, PartialEq)]
#[error("Unknown mbox format {0}, expected mboxo, mboxrd, mboxcl or mboxcl2")]
pub struct MboxFormatParseError(pub String);

/// One of the four mbox variants.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "lowercase"))]
pub enum MboxFormat {
    /// Original format: `From ` lines quoted with one `>`, lossy.
    MboxO,
    /// Reversible quoting of `>*From ` lines, the default for new
    /// messages.
    #[default]
    MboxRd,
    /// mboxo quoting plus a `Content-Length` header.
    MboxCl,
    /// No quoting, a `Content-Length` header delimits the body.
    MboxCl2,
}

impl MboxFormat {
    /// Returns `true` when the variant carries a `Content-Length` header.
    pub fn has_content_length(self) -> bool {
        matches!(self, Self::MboxCl | Self::MboxCl2)
    }

    /// Quotes the lines of `message` that a reader would take for a
    /// `From_` line.
    pub fn escape(self, message: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(message.len() + 16);
        for line in message.split_inclusive(|b| *b == b'\n') {
            if self.needs_quote(line) {
                out.push(b'>');
            }
            out.extend_from_slice(line);
        }
        out
    }

    /// Reverses [`Self::escape`] on a message read from a file.
    pub fn unescape(self, message: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(message.len());
        for line in message.split_inclusive(|b| *b == b'\n') {
            if self.is_quoted(line) {
                out.extend_from_slice(&line[1..]);
            } else {
                out.extend_from_slice(line);
            }
        }
        out
    }

    fn needs_quote(self, line: &[u8]) -> bool {
        match self {
            Self::MboxO | Self::MboxCl => line.starts_with(b"From "),
            Self::MboxRd => strip_quotes(line).starts_with(b"From "),
            Self::MboxCl2 => false,
        }
    }

    fn is_quoted(self, line: &[u8]) -> bool {
        match self {
            Self::MboxO | Self::MboxCl => line.starts_with(b">From "),
            Self::MboxRd => line.starts_with(b">") && strip_quotes(line).starts_with(b"From "),
            Self::MboxCl2 => false,
        }
    }
}

impl fmt::Display for MboxFormat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::MboxO => "mboxo",
            Self::MboxRd => "mboxrd",
            Self::MboxCl => "mboxcl",
            Self::MboxCl2 => "mboxcl2",
        })
    }
}

impl FromStr for MboxFormat {
    type Err = MboxFormatParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "mboxo" => Ok(Self::MboxO),
            "mboxrd" => Ok(Self::MboxRd),
            "mboxcl" => Ok(Self::MboxCl),
            "mboxcl2" => Ok(Self::MboxCl2),
            _ => Err(MboxFormatParseError(s.into())),
        }
    }
}

fn strip_quotes(line: &[u8]) -> &[u8] {
    let quotes = line.iter().take_while(|b| **b == b'>').count();
    &line[quotes..]
}

#[cfg(test)]
mod tests {
    use alloc::string::ToString;

    use crate::format::MboxFormat;

    const BODY: &[u8] = b"From a\n>From b\n>>From c\nFromage\n From d\n";

    #[test]
    fn mboxrd_round_trips() {
        let escaped = MboxFormat::MboxRd.escape(BODY);
        assert_eq!(
            escaped,
            b">From a\n>>From b\n>>>From c\nFromage\n From d\n".as_slice()
        );
        assert_eq!(MboxFormat::MboxRd.unescape(&escaped), BODY);
    }

    #[test]
    fn mboxo_quotes_bare_from_lines_only() {
        let escaped = MboxFormat::MboxO.escape(BODY);
        assert_eq!(
            escaped,
            b">From a\n>From b\n>>From c\nFromage\n From d\n".as_slice()
        );
        let unescaped = MboxFormat::MboxO.unescape(&escaped);
        assert_eq!(
            unescaped,
            b"From a\nFrom b\n>>From c\nFromage\n From d\n".as_slice()
        );
    }

    #[test]
    fn mboxcl2_is_verbatim() {
        assert_eq!(MboxFormat::MboxCl2.escape(BODY), BODY);
        assert_eq!(MboxFormat::MboxCl2.unescape(BODY), BODY);
        assert!(MboxFormat::MboxCl2.has_content_length());
        assert!(MboxFormat::MboxCl.has_content_length());
        assert!(!MboxFormat::MboxRd.has_content_length());
    }

    #[test]
    fn crlf_and_missing_final_newline() {
        let body = b"From a\r\nx\r\nFrom b";
        let escaped = MboxFormat::MboxRd.escape(body);
        assert_eq!(escaped, b">From a\r\nx\r\n>From b".as_slice());
        assert_eq!(MboxFormat::MboxRd.unescape(&escaped), body);
    }

    #[test]
    fn names_round_trip() {
        for format in [
            MboxFormat::MboxO,
            MboxFormat::MboxRd,
            MboxFormat::MboxCl,
            MboxFormat::MboxCl2,
        ] {
            assert_eq!(format.to_string().parse::<MboxFormat>(), Ok(format));
        }
        assert_eq!("MBOXRD".parse::<MboxFormat>(), Ok(MboxFormat::MboxRd));
        assert!("maildir".parse::<MboxFormat>().is_err());
    }
}
