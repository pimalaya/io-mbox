//! # Header
//!
//! Minimal RFC 5322 header-block helpers the crate needs without pulling
//! a MIME parser into the no_std core: field iteration with unfolding,
//! field lookup, and addr-spec extraction for the `From_` line sender.
//!
//! Refs: <https://datatracker.ietf.org/doc/html/rfc5322#section-2.2>

use alloc::{string::String, vec::Vec};

/// Iterates over the `(name, unfolded value)` fields of a header block.
///
/// Lines that are neither a field nor a continuation are skipped, so a
/// malformed block yields what it can.
pub(crate) fn fields(block: &[u8]) -> Vec<(&[u8], Vec<u8>)> {
    let mut out: Vec<(&[u8], Vec<u8>)> = Vec::new();

    for line in block.split_inclusive(|b| *b == b'\n') {
        let line = trim_eol(line);
        if line.is_empty() {
            break;
        }

        if matches!(line[0], b' ' | b'\t') {
            if let Some((_, value)) = out.last_mut() {
                value.push(b' ');
                value.extend_from_slice(line.trim_ascii());
            }
            continue;
        }

        if let Some(name) = field_name(line) {
            let value = line[name.len() + 1..].trim_ascii().to_vec();
            out.push((name, value));
        }
    }

    out
}

/// Returns the value of the first field named `name` (any case).
pub(crate) fn find(block: &[u8], name: &str) -> Option<Vec<u8>> {
    fields(block)
        .into_iter()
        .find(|(n, _)| n.eq_ignore_ascii_case(name.as_bytes()))
        .map(|(_, value)| value)
}

/// Returns the field name of a header line, when the line opens a
/// field: printable ASCII other than space up to the first colon.
pub(crate) fn field_name(line: &[u8]) -> Option<&[u8]> {
    let colon = line.iter().position(|b| *b == b':')?;
    let name = &line[..colon];
    if name.is_empty() || !name.iter().all(|b| (33..=126).contains(b)) {
        return None;
    }
    Some(name)
}

/// Extracts the addr-spec of an address field value: the part between
/// angle brackets, else the first whitespace-free token holding an `@`.
pub(crate) fn addr_spec(value: &[u8]) -> Option<String> {
    let value = String::from_utf8_lossy(value);

    if let Some(start) = value.find('<')
        && let Some(len) = value[start + 1..].find('>')
    {
        let addr = value[start + 1..start + 1 + len].trim();
        if !addr.is_empty() && !addr.contains(char::is_whitespace) {
            return Some(addr.into());
        }
    }

    value
        .split(|c: char| c.is_whitespace() || c == ',')
        .find(|token| token.contains('@'))
        .map(|token| {
            token
                .trim_matches(|c| c == '"' || c == '(' || c == ')')
                .into()
        })
}

/// Strips a trailing LF or CRLF.
pub(crate) fn trim_eol(line: &[u8]) -> &[u8] {
    let line = line.strip_suffix(b"\n").unwrap_or(line);
    line.strip_suffix(b"\r").unwrap_or(line)
}

#[cfg(test)]
mod tests {
    use crate::header::{addr_spec, field_name, fields, find};

    #[test]
    fn fields_unfold_continuations() {
        let block =
            b"Subject: a\r\n  b\r\nX-Keywords: one,\n\ttwo\nbroken line\nTo: c\n\nBody: no\n";
        let fields = fields(block);
        assert_eq!(fields.len(), 3);
        assert_eq!(fields[0], (b"Subject".as_slice(), b"a b".to_vec()));
        assert_eq!(fields[1], (b"X-Keywords".as_slice(), b"one, two".to_vec()));
        assert_eq!(fields[2], (b"To".as_slice(), b"c".to_vec()));
    }

    #[test]
    fn find_ignores_case() {
        let block = b"content-length: 12\n";
        assert_eq!(find(block, "Content-Length"), Some(b"12".to_vec()));
        assert_eq!(find(block, "Status"), None);
    }

    #[test]
    fn field_name_rejects_spaces() {
        assert_eq!(field_name(b"X-Status: F"), Some(b"X-Status".as_slice()));
        assert_eq!(field_name(b"From me: hi"), None);
        assert_eq!(field_name(b": empty"), None);
        assert_eq!(field_name(b"no colon"), None);
    }

    #[test]
    fn addr_spec_extraction() {
        assert_eq!(
            addr_spec(b"John <john@x.org>").as_deref(),
            Some("john@x.org")
        );
        assert_eq!(addr_spec(b"<>"), None);
        assert_eq!(
            addr_spec(b"john@x.org (John)").as_deref(),
            Some("john@x.org")
        );
        assert_eq!(addr_spec(b"\"a\" <b c>, d@e").as_deref(), Some("d@e"));
        assert_eq!(addr_spec(b"nobody"), None);
    }
}
