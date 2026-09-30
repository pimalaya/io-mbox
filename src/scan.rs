//! # Scan
//!
//! [`MboxScanner`], the streaming parser splitting an mbox into
//! [`MboxEntry`]s. It performs no I/O: the caller feeds it chunks of any
//! size and collects the entries it completes, so memory stays bounded
//! whatever the file size. [`crate::index::sync`] wraps it in a coroutine.
//!
//! A message starts at a `From_` line ([`MboxFromLine::parse`]) that
//! follows a blank line, or opens the file. The blank line before a
//! separator belongs to neither message. Bytes before the first `From_`
//! line are ignored. With [`MboxScannerOptions::content_length`] (the
//! mboxcl variants), `From_` lines inside the range a `Content-Length`
//! header declares do not split.
//!
//! Each message gets a content id: the SHA-256 of its bytes with the
//! volatile header fields left out (the flag fields, the c-client and
//! Thunderbird metadata, `Content-Length`), truncated to 80 bits and
//! base32-encoded. Rewriting flags, whoever does it, leaves the id
//! unchanged.

use core::mem;

use alloc::{collections::BTreeMap, format, string::String, vec::Vec};

use memchr::memchr;
use sha2::{Digest, Sha256};

use crate::{
    entry::{MboxEntry, MboxSpan},
    flag::{FLAG_FIELDS, MboxFlags},
    from_line::MboxFromLine,
    header,
};

/// Longest line kept whole. A longer line is streamed and can be neither
/// a separator nor a blank line.
const LINE_CAP: usize = 4096;

/// Longest header kept in [`MboxEntry::header`].
const HEADER_CAP: usize = 256 * 1024;

/// Longest run of volatile fields kept to read flags from.
const VOLATILE_CAP: usize = 64 * 1024;

/// Header fields left out of the content id, lowercase.
const VOLATILE_FIELDS: [&str; 10] = [
    "status",
    "x-status",
    "x-keywords",
    "x-uid",
    "content-length",
    "x-imap",
    "x-imapbase",
    "x-mozilla-status",
    "x-mozilla-status2",
    "x-mozilla-keys",
];

/// Options of a [`MboxScanner`].
#[derive(Clone, Debug, Default, Eq, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct MboxScannerOptions {
    /// Trust `Content-Length` headers (mboxcl, mboxcl2): no split inside
    /// the body range they declare.
    pub content_length: bool,
    /// Keep each message header in [`MboxEntry::header`].
    pub header: bool,
}

/// Streaming mbox parser.
#[derive(Debug)]
pub struct MboxScanner {
    opts: MboxScannerOptions,
    /// Offset of the next byte to feed.
    offset: u64,
    /// Offset of the next byte processed, behind `offset` by the bytes of
    /// a buffered partial line.
    processed: u64,
    /// Bytes of the current line, when it spans several chunks.
    line: Vec<u8>,
    /// Class of the current line once it overflowed [`LINE_CAP`].
    overflow: Option<Class>,
    /// Blank line not yet known to be content or a separator.
    pending_blank: Option<(u64, Vec<u8>)>,
    current: Option<Message>,
    /// How many messages each content hash was seen for.
    seen: BTreeMap<String, u32>,
    entries: Vec<MboxEntry>,
}

impl MboxScanner {
    /// Builds a scanner starting at `offset`, which must open a line
    /// that may be a `From_` line: the start of the file, or the `From_`
    /// line of an entry to rescan from.
    pub fn new(offset: u64, opts: MboxScannerOptions) -> Self {
        Self {
            opts,
            offset,
            processed: offset,
            line: Vec::new(),
            overflow: None,
            pending_blank: None,
            current: None,
            seen: BTreeMap::new(),
            entries: Vec::new(),
        }
    }

    /// Accounts for the ids of entries kept from a previous scan of the
    /// same file, so duplicates found after them are numbered on.
    pub fn seed<'a>(&mut self, ids: impl IntoIterator<Item = &'a str>) {
        for id in ids {
            let hash = id.split('-').next().unwrap_or(id);
            *self.seen.entry(hash.into()).or_default() += 1;
        }
    }

    /// Offset of the next byte to feed.
    pub fn offset(&self) -> u64 {
        self.offset
    }

    /// Feeds the next chunk, returning the entries it completed.
    pub fn feed(&mut self, chunk: &[u8]) -> Vec<MboxEntry> {
        self.offset += chunk.len() as u64;
        let mut rest = chunk;

        while !rest.is_empty() {
            let (piece, eol) = match memchr(b'\n', rest) {
                Some(i) => (&rest[..=i], true),
                None => (rest, false),
            };
            rest = &rest[piece.len()..];

            if let Some(class) = self.overflow {
                self.content(piece, class);
                if eol {
                    self.overflow = None;
                }
            } else if self.line.len() + piece.len() > LINE_CAP {
                let mut line = mem::take(&mut self.line);
                line.extend_from_slice(piece);
                self.line_overflow(&line);
                if eol {
                    self.overflow = None;
                }
            } else if eol && self.line.is_empty() {
                self.line_complete(piece);
            } else {
                self.line.extend_from_slice(piece);
                if eol {
                    let line = mem::take(&mut self.line);
                    self.line_complete(&line);
                    self.line = line;
                    self.line.clear();
                }
            }
        }

        mem::take(&mut self.entries)
    }

    /// Ends the scan at end of file, returning the entries left.
    pub fn finish(mut self) -> Vec<MboxEntry> {
        if !self.line.is_empty() {
            let line = mem::take(&mut self.line);
            self.line_complete(&line);
        }
        self.close();
        self.entries
    }

    fn line_complete(&mut self, line: &[u8]) {
        let start = self.processed;

        if header::trim_eol(line).is_empty() && line.ends_with(b"\n") {
            if self.current.is_some() {
                if let Some((offset, blank)) = self.pending_blank.take() {
                    self.blank(offset, &blank);
                }
                self.pending_blank = Some((start, line.to_vec()));
            }
            self.processed += line.len() as u64;
            return;
        }

        let protected = match &self.current {
            Some(m) => match m.protected_until {
                Some(end) => start < end,
                // NOTE: a blank line ending the header is still pending, the
                // body range it opens would start with this very line.
                None => {
                    self.opts.content_length
                        && m.in_header
                        && self.pending_blank.is_some()
                        && m.declared_len().is_some_and(|len| len > 0)
                }
            },
            None => false,
        };
        let may_split = self.current.is_none() || (self.pending_blank.is_some() && !protected);

        if may_split
            && line.starts_with(b"From ")
            && let Some(from) = MboxFromLine::parse(line)
        {
            self.close();
            self.current = Some(Message::new(start, line, from));
            self.processed += line.len() as u64;
            return;
        }

        if self.current.is_none() {
            self.processed += line.len() as u64;
            return;
        }

        self.flush_blank();
        let class = self.classify(line);
        self.content(line, class);
    }

    fn line_overflow(&mut self, prefix: &[u8]) {
        if self.current.is_none() {
            self.processed += prefix.len() as u64;
            self.overflow = Some(Class::Preamble);
            return;
        }

        self.flush_blank();
        let class = self.classify(prefix);
        self.content(prefix, class);
        self.overflow = Some(class);
    }

    fn flush_blank(&mut self) {
        if let Some((offset, blank)) = self.pending_blank.take() {
            self.blank(offset, &blank);
        }
    }

    /// Accounts for a blank line that turned out to be content.
    fn blank(&mut self, offset: u64, blank: &[u8]) {
        let content_length = self.opts.content_length;
        let Some(message) = self.current.as_mut() else {
            return;
        };

        message.hasher.update(blank);
        message.end = offset + blank.len() as u64;
        message.terminated = true;

        if message.in_header {
            message.in_header = false;
            message.header_len = offset - message.message_offset;
            if content_length && let Some(len) = message.declared_len() {
                message.protected_until = Some(message.end + len);
            }
        }
    }

    fn classify(&mut self, line: &[u8]) -> Class {
        let Some(message) = self.current.as_mut() else {
            return Class::Preamble;
        };

        if !message.in_header {
            return Class::Body;
        }

        if matches!(line.first(), Some(b' ' | b'\t')) {
            return message.field;
        }

        let class = match header::field_name(line) {
            Some(name) => {
                let name = String::from_utf8_lossy(name).to_ascii_lowercase();
                if name == "x-imap" {
                    message.pseudo = true;
                }
                if FLAG_FIELDS.contains(&name.as_str()) {
                    Class::Flag
                } else if VOLATILE_FIELDS.contains(&name.as_str()) {
                    Class::Volatile
                } else {
                    Class::Header
                }
            }
            None => Class::Header,
        };

        message.field = class;
        class
    }

    /// Accounts for content bytes of the current message.
    fn content(&mut self, bytes: &[u8], class: Class) {
        let start = self.processed;
        self.processed += bytes.len() as u64;

        let keep_header = self.opts.header;
        let Some(message) = self.current.as_mut() else {
            return;
        };

        match class {
            Class::Preamble => return,
            Class::Body | Class::Header => message.hasher.update(bytes),
            Class::Volatile | Class::Flag => {
                cap_extend(&mut message.volatile, bytes, VOLATILE_CAP);
            }
        }

        if class == Class::Flag {
            let offset = start - message.message_offset;
            match message.flag_spans.last_mut() {
                Some(span) if span.offset + span.len == offset => span.len += bytes.len() as u64,
                _ => message.flag_spans.push(MboxSpan {
                    offset,
                    len: bytes.len() as u64,
                }),
            }
        }

        if keep_header && class != Class::Body {
            cap_extend(&mut message.header, bytes, HEADER_CAP);
        }

        if matches!(class, Class::Body | Class::Header) {
            message.terminated = bytes.ends_with(b"\n");
        }
        message.end = self.processed;
    }

    fn close(&mut self) {
        self.pending_blank = None;
        let Some(mut message) = self.current.take() else {
            return;
        };

        // NOTE: a final line without newline hashes as if it had one, so
        // adding the newline (as a flag rewrite must) keeps the id.
        if !message.terminated {
            message.hasher.update(b"\n");
        }

        let hash = message.hasher.finalize();
        let hash = base32(&hash[..10]);
        let count = self.seen.entry(hash.clone()).or_default();
        *count += 1;
        let id = match *count {
            1 => hash,
            n => format!("{hash}-{n}"),
        };

        let len = message.end - message.message_offset;
        let header_len = if message.in_header {
            len
        } else {
            message.header_len
        };

        self.entries.push(MboxEntry {
            id,
            offset: message.offset,
            message_offset: message.message_offset,
            len,
            header_len,
            flags: MboxFlags::from_header(&message.volatile),
            crlf: message.crlf,
            flag_spans: message.flag_spans,
            sender: message.from.sender,
            timestamp: message.from.timestamp,
            header: message.header,
            pseudo: message.pseudo,
        });
    }
}

/// Role of a line in the message being scanned.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Class {
    /// Before the first `From_` line.
    Preamble,
    /// A header field hashed into the id.
    Header,
    /// A header field left out of the id.
    Volatile,
    /// A flag field, left out of the id and located for rewrites.
    Flag,
    /// After the header.
    Body,
}

#[derive(Debug)]
struct Message {
    offset: u64,
    message_offset: u64,
    from: MboxFromLine,
    crlf: bool,
    hasher: Sha256,
    /// Offset right after the last content byte.
    end: u64,
    /// Whether the content so far ends with a newline.
    terminated: bool,
    in_header: bool,
    header_len: u64,
    /// Class of the header field in progress, for its continuations.
    field: Class,
    header: Vec<u8>,
    volatile: Vec<u8>,
    flag_spans: Vec<MboxSpan>,
    /// End of the body range a trusted `Content-Length` declares.
    protected_until: Option<u64>,
    pseudo: bool,
}

impl Message {
    /// Body length a `Content-Length` field declares.
    fn declared_len(&self) -> Option<u64> {
        let value = header::find(&self.volatile, "content-length")?;
        String::from_utf8_lossy(&value).trim().parse().ok()
    }

    fn new(offset: u64, line: &[u8], from: MboxFromLine) -> Self {
        let message_offset = offset + line.len() as u64;
        Self {
            offset,
            message_offset,
            from,
            crlf: line.ends_with(b"\r\n"),
            hasher: Sha256::new(),
            end: message_offset,
            terminated: true,
            in_header: true,
            header_len: 0,
            field: Class::Header,
            header: Vec::new(),
            volatile: Vec::new(),
            flag_spans: Vec::new(),
            protected_until: None,
            pseudo: false,
        }
    }
}

fn cap_extend(buf: &mut Vec<u8>, bytes: &[u8], cap: usize) {
    let room = cap.saturating_sub(buf.len());
    buf.extend_from_slice(&bytes[..bytes.len().min(room)]);
}

/// Lowercase RFC 4648 base32 without padding.
fn base32(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 32] = b"abcdefghijklmnopqrstuvwxyz234567";
    let mut out = String::with_capacity(bytes.len().div_ceil(5) * 8);
    let mut buffer = 0u16;
    let mut bits = 0;
    for byte in bytes {
        buffer = (buffer << 8) | *byte as u16;
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            out.push(ALPHABET[((buffer >> bits) & 31) as usize] as char);
        }
    }
    if bits > 0 {
        out.push(ALPHABET[((buffer << (5 - bits)) & 31) as usize] as char);
    }
    out
}

#[cfg(test)]
mod tests {
    use alloc::vec::Vec;

    use crate::{
        entry::{MboxEntry, MboxSpan},
        flag::MboxFlag,
        scan::{MboxScanner, MboxScannerOptions, base32},
    };

    fn scan(bytes: &[u8], chunk: usize, opts: MboxScannerOptions) -> Vec<MboxEntry> {
        let mut scanner = MboxScanner::new(0, opts);
        let mut entries = Vec::new();
        for piece in bytes.chunks(chunk.max(1)) {
            entries.extend(scanner.feed(piece));
        }
        entries.extend(scanner.finish());
        entries
    }

    fn scan_all_chunk_sizes(bytes: &[u8], opts: MboxScannerOptions) -> Vec<MboxEntry> {
        let reference = scan(bytes, bytes.len().max(1), opts.clone());
        for chunk in [1, 2, 3, 7, 64, 4095, 4096, 4097] {
            assert_eq!(scan(bytes, chunk, opts.clone()), reference, "chunk {chunk}");
        }
        reference
    }

    fn message<'a>(bytes: &'a [u8], entry: &MboxEntry) -> &'a [u8] {
        &bytes[entry.message_offset as usize..entry.end() as usize]
    }

    const MBOX: &[u8] = b"\
From a@b Mon Jan  1 00:00:00 2024
Subject: one
Status: RO

body one
From the man page
>From quoted

From c@d Tue Jan  2 00:00:00 2024
Subject: two
X-Status: F

body two

";

    #[test]
    fn splits_on_from_lines_after_blank_lines() {
        let entries = scan_all_chunk_sizes(MBOX, Default::default());
        assert_eq!(entries.len(), 2);

        assert_eq!(entries[0].offset, 0);
        assert_eq!(entries[0].sender, "a@b");
        assert_eq!(
            message(MBOX, &entries[0]),
            b"Subject: one\nStatus: RO\n\nbody one\nFrom the man page\n>From quoted\n".as_slice()
        );
        assert_eq!(entries[0].header_len, 24);
        assert!(entries[0].flags.contains(&MboxFlag::Seen));
        assert_eq!(
            entries[0].flag_spans,
            [MboxSpan {
                offset: 13,
                len: 11
            }]
        );

        assert_eq!(
            message(MBOX, &entries[1]),
            b"Subject: two\nX-Status: F\n\nbody two\n".as_slice()
        );
        assert_eq!(entries[1].timestamp, 1_704_153_600);
        assert!(entries[1].flags.contains(&MboxFlag::Flagged));
    }

    #[test]
    fn ids_ignore_volatile_fields() {
        let a = b"From a@b Mon Jan  1 00:00:00 2024\nSubject: x\n\nbody\n";
        let b = b"From z@z Tue Jan  2 00:00:00 2024\nStatus: RO\nSubject: x\nX-UID: 4\nX-Keywords: a,\n b\n\nbody\n";
        let c = b"From a@b Mon Jan  1 00:00:00 2024\nSubject: y\n\nbody\n";
        let a = scan(a, 4096, Default::default());
        let b = scan(b, 4096, Default::default());
        let c = scan(c, 4096, Default::default());
        assert_eq!(a[0].id, b[0].id);
        assert_ne!(a[0].id, c[0].id);
        assert_eq!(a[0].id.len(), 16);
        assert_eq!(b[0].flag_spans.len(), 2);
    }

    #[test]
    fn duplicates_are_numbered() {
        let one = b"From a@b Mon Jan  1 00:00:00 2024\nSubject: x\n\nbody\n\n";
        let mut bytes = Vec::new();
        for _ in 0..3 {
            bytes.extend_from_slice(one);
        }
        let entries = scan_all_chunk_sizes(&bytes, Default::default());
        let hash = entries[0].id.clone();
        assert_eq!(entries[1].id, alloc::format!("{hash}-2"));
        assert_eq!(entries[2].id, alloc::format!("{hash}-3"));

        let mut scanner = MboxScanner::new(0, Default::default());
        scanner.seed([hash.as_str(), entries[1].id.as_str()]);
        let mut resumed = scanner.feed(one);
        resumed.extend(scanner.finish());
        assert_eq!(resumed[0].id, entries[2].id);
    }

    #[test]
    fn from_lines_need_a_blank_line_before() {
        let bytes =
            b"From a@b Mon Jan  1 00:00:00 2024\n\nline\nFrom c@d Mon Jan  1 00:00:00 2024\n";
        let entries = scan_all_chunk_sizes(bytes, Default::default());
        assert_eq!(entries.len(), 1);
    }

    #[test]
    fn preamble_and_crlf() {
        let bytes = b"garbage\r\nFrom a@b Mon Jan  1 00:00:00 2024\r\nSubject: x\r\n\r\nbody\r\n\r\nFrom c@d Mon Jan  1 00:00:00 2024\r\n\r\n";
        let entries = scan_all_chunk_sizes(bytes, Default::default());
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].offset, 9);
        assert!(entries[0].crlf);
        assert_eq!(
            message(bytes, &entries[0]),
            b"Subject: x\r\n\r\nbody\r\n".as_slice()
        );
        assert_eq!(entries[0].header_len, 12);
        assert_eq!(entries[1].len, 0);
        assert_eq!(entries[1].header_len, 0);
    }

    #[test]
    fn missing_final_newline_and_empty_input() {
        let bytes = b"From a@b Mon Jan  1 00:00:00 2024\nSubject: x\n\nbody";
        let entries = scan_all_chunk_sizes(bytes, Default::default());
        assert_eq!(
            message(bytes, &entries[0]),
            b"Subject: x\n\nbody".as_slice()
        );
        assert!(scan(b"", 1, Default::default()).is_empty());
        assert!(scan(b"not an mbox\n\n", 1, Default::default()).is_empty());

        let bytes = b"From a@b Mon Jan  1 00:00:00 2024\nSubject: x";
        let entries = scan_all_chunk_sizes(bytes, Default::default());
        assert_eq!(entries[0].header_len, entries[0].len);
    }

    #[test]
    fn content_length_protects_the_body() {
        let bytes = b"From a@b Mon Jan  1 00:00:00 2024\nContent-Length: 39\n\nx\n\nFrom c@d Mon Jan  1 00:00:00 2024\nz\n\nFrom e@f Mon Jan  1 00:00:00 2024\n\n";
        let opts = MboxScannerOptions {
            content_length: true,
            ..Default::default()
        };
        assert_eq!(scan_all_chunk_sizes(bytes, opts).len(), 2);
        assert_eq!(scan_all_chunk_sizes(bytes, Default::default()).len(), 3);

        let bytes = b"From a@b Mon Jan  1 00:00:00 2024\nContent-Length: 35\n\nFrom c@d Mon Jan  1 00:00:00 2024\n\nFrom e@f Mon Jan  1 00:00:00 2024\nContent-Length: 0\n\nFrom g@h Mon Jan  1 00:00:00 2024\n";
        let opts = MboxScannerOptions {
            content_length: true,
            ..Default::default()
        };
        assert_eq!(scan_all_chunk_sizes(bytes, opts).len(), 3);
    }

    #[test]
    fn long_lines_stream() {
        let mut bytes = b"From a@b Mon Jan  1 00:00:00 2024\nX-Long: ".to_vec();
        bytes.extend(core::iter::repeat_n(b'x', 10_000));
        bytes.extend_from_slice(b"\nStatus: R\n\n");
        bytes.extend(core::iter::repeat_n(b'y', 10_000));
        bytes.extend_from_slice(b"\n\nFrom c@d Mon Jan  1 00:00:00 2024\n\n");
        let mut preamble = core::iter::repeat_n(b'z', 5000).collect::<Vec<u8>>();
        preamble.push(b'\n');
        preamble.extend_from_slice(&bytes);

        let opts = MboxScannerOptions {
            header: true,
            ..Default::default()
        };
        let entries = scan_all_chunk_sizes(&preamble, opts);
        assert_eq!(entries.len(), 2);
        assert!(entries[0].flags.contains(&MboxFlag::Seen));
        assert_eq!(entries[0].header.len() as u64, entries[0].header_len);
        assert_eq!(entries[0].len, 10_009 + 10 + 1 + 10_001);
    }

    #[test]
    fn pseudo_message_is_marked() {
        let bytes = b"From MAILER_DAEMON Mon Jan  1 00:00:00 2024\nSubject: DON'T DELETE THIS MESSAGE -- FOLDER INTERNAL DATA\nX-IMAP: 1 2\n\nx\n\nFrom a@b Mon Jan  1 00:00:00 2024\nX-IMAPbase: 1 2\n\n";
        let entries = scan(bytes, 4096, Default::default());
        assert!(entries[0].pseudo);
        assert!(!entries[1].pseudo);
    }

    #[test]
    fn base32_encodes_rfc4648_vectors() {
        assert_eq!(base32(b""), "");
        assert_eq!(base32(b"f"), "my");
        assert_eq!(base32(b"fooba"), "mzxw6ytb");
        assert_eq!(base32(b"foobar"), "mzxw6ytboi");
    }

    /// Checks what must hold whatever the input: chunking does not matter,
    /// entries lie in order inside the input, and each one scans back to
    /// itself when its own range is scanned alone, which is what a read
    /// relies on.
    fn check_invariants(bytes: &[u8], opts: MboxScannerOptions) {
        let entries = scan(bytes, bytes.len().max(1), opts.clone());
        for chunk in [1, 3, 64] {
            assert_eq!(scan(bytes, chunk, opts.clone()), entries, "chunk {chunk}");
        }

        let mut previous_end = 0;
        for entry in &entries {
            assert!(entry.offset >= previous_end);
            assert!(entry.offset < entry.message_offset);
            assert!(entry.end() <= bytes.len() as u64);
            assert!(entry.header_len <= entry.len);
            for span in &entry.flag_spans {
                assert!(span.offset + span.len <= entry.header_len);
            }
            previous_end = entry.end();

            let range = &bytes[entry.offset as usize..entry.end() as usize];
            let mut scanner = MboxScanner::new(entry.offset, opts.clone());
            let mut alone = scanner.feed(range);
            if range.ends_with(b"\n") {
                alone.extend(scanner.feed(b"\n"));
            }
            alone.extend(scanner.finish());
            assert_eq!(alone.len(), 1);
            assert_eq!(alone[0].id, entry.id.split('-').next().unwrap());
            assert_eq!(alone[0].len, entry.len);
        }
    }

    #[test]
    fn random_inputs_hold_invariants() {
        const TOKENS: [&[u8]; 16] = [
            b"From ",
            b"a@b",
            b" Mon Jan  1 00:00:00 2024",
            b"\n",
            b"\n",
            b"\r\n",
            b">",
            b"Status: RO",
            b"X-Status: F",
            b"Content-Length: 3",
            b"X-Keywords: a",
            b"Subject: s",
            b" ",
            b"x",
            b"\n\nFrom a@b Mon Jan  1 00:00:00 2024\n",
            b"X-IMAP: 1",
        ];

        let mut state = 0x2545_f491_4f6c_dd1du64;
        let mut next = || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };

        for _ in 0..3000 {
            let len = next() % 60;
            let mut bytes = Vec::new();
            for _ in 0..len {
                bytes.extend_from_slice(TOKENS[(next() % TOKENS.len() as u64) as usize]);
            }
            let content_length = next() % 2 == 0;
            let opts = MboxScannerOptions {
                content_length,
                header: true,
            };
            check_invariants(&bytes, opts);
        }
    }
}
