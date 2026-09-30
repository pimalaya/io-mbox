//! Corpus suite: invariants every mbox must satisfy, with no hand-written
//! expectation, so any mbox file can be dropped in.
//!
//! `fixtures` runs on the small files committed under tests/fixtures.
//! `corpus` (ignored by default) runs on the public archives
//! tests/corpus/fetch.sh downloads into target/mbox-corpus, plus every
//! `.mbox` file of the directories listed in `IO_MBOX_CORPUS_DIR`
//! (colon-separated), meant for private mailboxes. Files are always
//! copied to a temporary directory first: the originals are never
//! written.
//!
//! ```sh
//! tests/corpus/fetch.sh
//! cargo test --test corpus -- --ignored --nocapture
//! ```
//!
//! For each file:
//!
//! 1. the Message-ID sequence matches the one of mail-parser's mbox
//!    iterator, spurious splits of the latter (on body lines starting
//!    with `From `) aside;
//! 2. scanning in chunks of any size yields the same entries;
//! 3. flagging every third message keeps every id and every byte outside
//!    the flag fields, and restoring the flags reads them back;
//! 4. removing every fifth message keeps the others, ids and bytes;
//! 5. appending every message to an empty mbox, in each write format,
//!    reads back the same messages;
//! 6. an index built on the first half of the file and resumed after
//!    the second half is appended equals a full scan.

use std::{
    collections::BTreeMap,
    env,
    fs::{self, File},
    io::{BufReader, Write},
    path::{Path, PathBuf},
    time::Instant,
};

use io_mbox::{
    client::MboxClient,
    entry::{MboxEntry, append::MboxEntryAppendItem, update::MboxEntryUpdateEdit},
    flag::MboxFlag,
    format::MboxFormat,
    index::sync::MboxIndexSyncOutcome,
    lock::MboxLockOptions,
    scan::{MboxScanner, MboxScannerOptions},
};
use mail_parser::{MessageParser, mailbox::mbox::MessageIterator};
use tempfile::TempDir;

#[test]
fn fixtures() {
    let _ = env_logger::try_init();
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let files = mbox_files(&dir);
    assert!(files.len() >= 10, "fixtures missing");
    for file in files {
        check(&file);
    }
}

#[test]
#[ignore = "needs tests/corpus/fetch.sh"]
fn corpus() {
    let _ = env_logger::try_init();
    let files: Vec<PathBuf> = corpus_dirs()
        .iter()
        .flat_map(|dir| mbox_files(dir))
        .collect();
    assert!(
        !files.is_empty(),
        "no corpus found, run tests/corpus/fetch.sh first"
    );
    for file in files {
        check(&file);
    }
}

#[test]
#[ignore = "needs tests/corpus/fetch.sh, writes about 200 MB"]
fn large_file_streams() {
    let source = corpus_dirs()
        .into_iter()
        .map(|dir| dir.join("lore_git_2024_01.mbox"))
        .find(|path| path.is_file())
        .expect("run tests/corpus/fetch.sh first");
    let bytes = fs::read(&source).unwrap();
    let copies = 10;

    let tmp = TempDir::new().unwrap();
    let mut big = File::create(tmp.path().join("big")).unwrap();
    for _ in 0..copies {
        big.write_all(&bytes).unwrap();
    }
    drop(big);

    let client = client(&tmp, false);
    let one = client_scan(&bytes, 1 << 16, false).len();

    let start = Instant::now();
    let index = client.sync_index("big", None).unwrap().index;
    println!(
        "synced {} MB in {:?}",
        (bytes.len() * copies) >> 20,
        start.elapsed()
    );

    assert_eq!(index.entries.len(), one * copies);
    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
    for entry in &index.entries {
        *counts
            .entry(entry.id.split('-').next().unwrap())
            .or_default() += 1;
    }
    assert!(counts.values().all(|n| n % copies == 0));
}

/// target/mbox-corpus, where fetch.sh downloads, then the directories
/// listed in `IO_MBOX_CORPUS_DIR`.
fn corpus_dirs() -> Vec<PathBuf> {
    let mut dirs = vec![Path::new(env!("CARGO_MANIFEST_DIR")).join("target/mbox-corpus")];
    if let Ok(extra) = env::var("IO_MBOX_CORPUS_DIR") {
        dirs.extend(
            extra
                .split(':')
                .filter(|d| !d.is_empty())
                .map(PathBuf::from),
        );
    }
    dirs
}

fn mbox_files(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut files: Vec<PathBuf> = entries
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|e| e == "mbox"))
        .collect();
    files.sort();
    files
}

fn client(tmp: &TempDir, content_length: bool) -> MboxClient {
    let mut client = MboxClient::new(tmp.path());
    client.scanner.content_length = content_length;
    client.lock = MboxLockOptions {
        timeout_secs: Some(1),
        ..Default::default()
    };
    client.temp_dir = Some(tmp.path().join("tmp"));
    fs::create_dir_all(tmp.path().join("tmp")).unwrap();
    client
}

fn client_scan(bytes: &[u8], chunk: usize, content_length: bool) -> Vec<MboxEntry> {
    let opts = MboxScannerOptions {
        content_length,
        ..Default::default()
    };
    let mut scanner = MboxScanner::new(0, opts);
    let mut entries = Vec::new();
    for piece in bytes.chunks(chunk) {
        entries.extend(scanner.feed(piece));
    }
    entries.extend(scanner.finish());
    entries
}

fn check(path: &Path) {
    let name = path.file_name().unwrap().to_string_lossy().into_owned();
    let content_length = name.contains("mboxcl");
    let bytes = fs::read(path).unwrap();
    let start = Instant::now();

    let tmp = TempDir::new().unwrap();
    fs::write(tmp.path().join("orig"), &bytes).unwrap();
    let client = client(&tmp, content_length);

    let index = client.sync_index("orig", None).unwrap().index;
    let entries = index.entries.clone();
    let messages: Vec<Vec<u8>> = entries
        .iter()
        .map(|entry| client.get("orig", entry.clone()).unwrap().contents)
        .collect();

    // 1. differential against mail-parser
    if !content_length {
        let oracle: Vec<String> = MessageIterator::new(BufReader::new(&bytes[..]))
            .filter_map(|m| message_id(m.unwrap().contents()))
            .collect();
        let ours: Vec<String> = messages.iter().filter_map(|m| message_id(m)).collect();
        assert_eq!(
            ours, oracle,
            "{name}: Message-ID sequence differs from mail-parser"
        );
    }

    // 2. chunk-size independence
    let reference = client_scan(&bytes, bytes.len().max(1), content_length);
    assert_eq!(reference, entries, "{name}: client scan");
    let chunks: &[usize] = if bytes.len() < 1 << 20 {
        &[1, 7, 4096, 1 << 20]
    } else {
        &[7, 4096, 1 << 20]
    };
    for chunk in chunks {
        assert_eq!(
            client_scan(&bytes, *chunk, content_length),
            reference,
            "{name}: chunk {chunk}"
        );
    }

    // 3. flag rewrite and restore
    let flagged: BTreeMap<String, MboxEntryUpdateEdit> = entries
        .iter()
        .step_by(3)
        .map(|entry| {
            let mut flags = entry.flags.clone();
            flags.insert(MboxFlag::Flagged);
            flags.insert(MboxFlag::Keyword("io-mbox".into()));
            (entry.id.clone(), MboxEntryUpdateEdit::Flags(flags))
        })
        .collect();
    let out = client
        .update("orig", flagged.clone(), Some(index.clone()))
        .unwrap();
    let rewritten = fs::read(tmp.path().join("orig")).unwrap();
    let fresh = client_scan(&rewritten, 1 << 16, content_length);
    assert_eq!(out.index.entries, fresh, "{name}: index after flag rewrite");
    assert_eq!(ids(&fresh), ids(&entries), "{name}: ids after flag rewrite");
    for (entry, before) in fresh.iter().zip(&entries) {
        if let Some(MboxEntryUpdateEdit::Flags(flags)) = flagged.get(&entry.id) {
            assert_eq!(&entry.flags, flags, "{name}: flags of {}", entry.id);
        } else {
            assert_eq!(
                entry.flags, before.flags,
                "{name}: untouched flags of {}",
                entry.id
            );
        }
    }
    assert_eq!(
        strip_flags(&rewritten, &fresh),
        strip_flags(&bytes, &entries),
        "{name}: bytes outside flag fields"
    );

    let restore: BTreeMap<String, MboxEntryUpdateEdit> = entries
        .iter()
        .step_by(3)
        .map(|entry| {
            (
                entry.id.clone(),
                MboxEntryUpdateEdit::Flags(entry.flags.clone()),
            )
        })
        .collect();
    let out = client.update("orig", restore, Some(out.index)).unwrap();
    let restored = fs::read(tmp.path().join("orig")).unwrap();
    let fresh = client_scan(&restored, 1 << 16, content_length);
    assert_eq!(out.index.entries, fresh, "{name}: index after restore");
    let flags: Vec<_> = fresh.iter().map(|e| e.flags.clone()).collect();
    let original: Vec<_> = entries.iter().map(|e| e.flags.clone()).collect();
    assert_eq!(flags, original, "{name}: restored flags");
    assert_eq!(
        strip_flags(&restored, &fresh),
        strip_flags(&bytes, &entries),
        "{name}: bytes after restore"
    );

    // 4. removal
    fs::write(tmp.path().join("orig"), &bytes).unwrap();
    let removed: BTreeMap<String, MboxEntryUpdateEdit> = entries
        .iter()
        .step_by(5)
        .map(|entry| (entry.id.clone(), MboxEntryUpdateEdit::Remove))
        .collect();
    let out = client.update("orig", removed.clone(), None).unwrap();
    assert_eq!(out.removed, removed.len(), "{name}: removed count");
    let after = fs::read(tmp.path().join("orig")).unwrap();
    let fresh = client_scan(&after, 1 << 16, content_length);
    assert_eq!(out.index.entries, fresh, "{name}: index after removal");
    let kept: Vec<(&MboxEntry, &Vec<u8>)> = entries
        .iter()
        .zip(&messages)
        .filter(|(e, _)| !removed.contains_key(&e.id))
        .collect();
    assert_eq!(fresh.len(), kept.len(), "{name}: count after removal");
    for (entry, (before, contents)) in fresh.iter().zip(&kept).step_by(7) {
        assert_eq!(
            hash(&entry.id),
            hash(&before.id),
            "{name}: id after removal"
        );
        let got = client.get("orig", entry.clone()).unwrap().contents;
        assert_eq!(&got, *contents, "{name}: contents after removal");
    }

    // 5. append round trip, per write format
    for format in [MboxFormat::MboxO, MboxFormat::MboxRd, MboxFormat::MboxCl2] {
        let target = format!("append-{format}");
        client.create_mbox(target.as_str()).unwrap();
        let mut writer = client.clone();
        writer.format = format;
        writer.scanner.content_length = format == MboxFormat::MboxCl2;

        let items: Vec<MboxEntryAppendItem> = entries
            .iter()
            .zip(&messages)
            .map(|(entry, contents)| MboxEntryAppendItem {
                contents: contents.clone(),
                flags: entry.flags.clone(),
                sender: Some(entry.sender.clone()),
                timestamp: Some(entry.timestamp),
            })
            .collect();
        for chunk in items.chunks(100) {
            writer.append(target.as_str(), chunk.to_vec()).unwrap();
        }

        let index = writer.sync_index(target.as_str(), None).unwrap().index;
        assert_eq!(index.entries.len(), entries.len(), "{name}: {format} count");
        for ((entry, original), before) in index.entries.iter().zip(&messages).zip(&entries) {
            let got = writer.get(target.as_str(), entry.clone()).unwrap().contents;
            assert_eq!(entry.flags, before.flags, "{name}: {format} flags");
            let sender = if before.sender.is_empty() || before.sender.contains(char::is_whitespace)
            {
                "MAILER-DAEMON"
            } else {
                before.sender.as_str()
            };
            assert_eq!(entry.sender, sender, "{name}: {format} sender");
            if format == MboxFormat::MboxO && has_quoted_from(original) {
                continue;
            }
            assert_eq!(
                normalize(&got),
                normalize(original),
                "{name}: {format} contents of {}",
                before.id
            );
        }
    }

    // 6. index resume
    if entries.len() >= 2 {
        let half = entries[entries.len() / 2].offset as usize;
        fs::write(tmp.path().join("grow"), &bytes[..half]).unwrap();
        let first = client.sync_index("grow", None).unwrap().index;
        fs::write(tmp.path().join("grow"), &bytes).unwrap();
        let out = client.sync_index("grow", Some(first)).unwrap();
        assert_eq!(
            out.outcome,
            MboxIndexSyncOutcome::Resumed,
            "{name}: resume outcome"
        );
        assert_eq!(out.index.entries, entries, "{name}: resumed index");
    }

    println!(
        "{name}: {} messages, {} bytes, ok in {:?}",
        entries.len(),
        bytes.len(),
        start.elapsed()
    );
}

fn message_id(contents: &[u8]) -> Option<String> {
    MessageParser::default()
        .parse(contents)?
        .message_id()
        .map(Into::into)
}

fn ids(entries: &[MboxEntry]) -> Vec<&str> {
    entries.iter().map(|e| e.id.as_str()).collect()
}

fn hash(id: &str) -> &str {
    id.split('-').next().unwrap()
}

/// The file bytes with every flag field removed.
fn strip_flags(bytes: &[u8], entries: &[MboxEntry]) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes.len());
    let mut cursor = 0;
    for entry in entries {
        for span in &entry.flag_spans {
            let start = (entry.message_offset + span.offset) as usize;
            out.extend_from_slice(&bytes[cursor..start]);
            cursor = start + span.len as usize;
        }
    }
    out.extend_from_slice(&bytes[cursor..]);
    out
}

/// A message as an append stores it: flag fields and `Content-Length`
/// dropped, a blank line after a header lacking one, a final newline,
/// LF line endings.
fn normalize(message: &[u8]) -> Vec<u8> {
    let mut lf = Vec::with_capacity(message.len());
    for (i, byte) in message.iter().enumerate() {
        if *byte != b'\r' || message.get(i + 1) != Some(&b'\n') {
            lf.push(*byte);
        }
    }
    let message = lf.as_slice();
    let mut out = Vec::with_capacity(message.len());
    let mut in_header = true;
    let mut dropping = false;
    let mut had_blank = false;
    for line in message.split_inclusive(|b| *b == b'\n') {
        if in_header {
            if line == b"\n" || line == b"\r\n" {
                in_header = false;
                had_blank = true;
                out.extend_from_slice(line);
                continue;
            }
            if !line.starts_with(b" ") && !line.starts_with(b"\t") {
                let name = line
                    .split(|b| *b == b':')
                    .next()
                    .unwrap_or_default()
                    .to_ascii_lowercase();
                dropping = [
                    &b"status"[..],
                    b"x-status",
                    b"x-keywords",
                    b"content-length",
                ]
                .contains(&name.as_slice());
            }
            if dropping {
                continue;
            }
        }
        out.extend_from_slice(line);
    }
    if !out.ends_with(b"\n") {
        out.push(b'\n');
    }
    if !had_blank {
        out.push(b'\n');
    }
    out
}

fn has_quoted_from(message: &[u8]) -> bool {
    message.split(|b| *b == b'\n').any(|line| {
        let quotes = line.iter().take_while(|b| **b == b'>').count();
        quotes > 0 && line[quotes..].starts_with(b"From ")
    })
}
