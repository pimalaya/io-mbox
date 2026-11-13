//! End-to-end flow of the std client against a temporary directory:
//!
//! ```text
//! LIST (empty) → CREATE inbox, archive, lists/rust → LIST
//!   → APPEND x3 → SYNC (rebuilt) → SYNC (reused) → GET
//!   → UPDATE flags → GET (flags read back) → COPY → MOVE
//!   → deliver behind the client's back → SYNC (resumed)
//!   → RENAME → DELETE → LIST
//! ```
//!
//! Then the locks against a concurrent writer: a busy dotlock, a stale
//! one, a busy fcntl lock held through another open file description.

use std::{
    collections::BTreeMap,
    fs::{self, OpenOptions},
    io::Write,
    time::{Duration, SystemTime},
};

use io_mbox::{
    client::{MboxClient, MboxClientError},
    entry::append::MboxEntryAppendError,
    entry::{append::MboxEntryAppendItem, update::MboxEntryUpdateEdit},
    flag::{MboxFlag, MboxFlags, MboxKeyword},
    index::sync::MboxIndexSyncOutcome,
    lock::{MboxLockOptions, acquire::MboxLockAcquireError},
};
use tempfile::{TempDir, tempdir};

fn client(tmp: &TempDir) -> MboxClient {
    let mut client = MboxClient::new(tmp.path().join("mail"));
    client.store.inbox = Some(tmp.path().join("spool").into());
    client.temp_dir = Some(tmp.path().to_path_buf());
    client.lock.timeout_secs = Some(1);
    client
}

fn item(subject: &str) -> MboxEntryAppendItem {
    let contents = format!(
        "From: Alice <alice@example.org>\nMessage-ID: <{subject}@example.org>\nSubject: {subject}\n\nFrom the start of a body line\n"
    );
    MboxEntryAppendItem {
        contents: contents.into_bytes(),
        ..Default::default()
    }
}

#[test]
fn end_to_end() {
    let _ = env_logger::try_init();
    let tmp = tempdir().unwrap();
    let client = client(&tmp);

    assert!(client.list_mboxes().unwrap().is_empty());

    fs::write(tmp.path().join("spool"), b"").unwrap();
    client.create_mbox("archive").unwrap();
    client.create_mbox("lists/rust").unwrap();
    assert!(matches!(
        client.create_mbox("archive"),
        Err(MboxClientError::Create(_))
    ));

    let names: Vec<String> = client
        .list_mboxes()
        .unwrap()
        .into_iter()
        .map(|m| m.name.to_string())
        .collect();
    assert_eq!(names, ["INBOX", "archive", "lists/rust"]);

    let appended = client
        .append("INBOX", vec![item("one"), item("two"), item("three")])
        .unwrap();
    assert_eq!(appended.len(), 3);
    assert_eq!(appended[0].sender, "alice@example.org");

    let out = client.sync_index("INBOX", None).unwrap();
    assert_eq!(out.outcome, MboxIndexSyncOutcome::Rebuilt);
    let index = out.index;
    assert_eq!(index.entries, appended);
    let again = client.sync_index("INBOX", Some(index.clone())).unwrap();
    assert_eq!(again.outcome, MboxIndexSyncOutcome::Reused);

    let one = client
        .get_by_id("INBOX", &appended[0].id, Some(index.clone()))
        .unwrap();
    assert!(
        one.contents
            .ends_with(b"\n\nFrom the start of a body line\n")
    );
    assert!(one.parsed().unwrap().subject() == Some("one"));
    let spool = fs::read(tmp.path().join("spool")).unwrap();
    assert!(spool.windows(9).any(|w| w == b"\n>From th"));

    let flags = MboxFlags::from_iter([MboxFlag::Seen, MboxFlag::Flagged]);
    let edits = BTreeMap::from([(
        appended[1].id.clone(),
        MboxEntryUpdateEdit::Flags(flags.clone()),
    )]);
    let out = client.update("INBOX", edits, Some(index)).unwrap();
    assert_eq!(out.updated, 1);
    let two = client.get("INBOX", out.index.entries[1].clone()).unwrap();
    assert_eq!(two.entry.flags, flags);
    assert!(String::from_utf8_lossy(&two.contents).contains("Status: R\nX-Status: F\n"));

    let copied = client
        .copy("INBOX", "archive", vec![out.index.entries[1].clone()])
        .unwrap();
    assert_eq!(copied[0].id, appended[1].id);
    assert_eq!(copied[0].flags, flags);

    let moved = client
        .r#move(
            "INBOX",
            "lists/rust",
            vec![out.index.entries[0].clone()],
            Some(out.index.clone()),
        )
        .unwrap();
    assert_eq!(moved.entries[0].id, appended[0].id);
    assert_eq!(moved.index.entries.len(), 2);

    let mut spool = OpenOptions::new()
        .append(true)
        .open(tmp.path().join("spool"))
        .unwrap();
    spool
        .write_all(b"From mta@example.org Mon Jan  1 00:00:00 2024\nSubject: delivered\n\nhi\n\n")
        .unwrap();
    drop(spool);
    let out = client.sync_index("INBOX", Some(moved.index)).unwrap();
    assert_eq!(out.outcome, MboxIndexSyncOutcome::Resumed);
    assert_eq!(out.index.entries.len(), 3);
    assert_eq!(out.index, client.sync_index("INBOX", None).unwrap().index);

    client.rename_mbox("lists/rust", "lists/rustlang").unwrap();
    assert!(matches!(
        client.rename_mbox("nope", "x"),
        Err(MboxClientError::Rename(_))
    ));
    client.delete_mbox("archive").unwrap();
    assert!(matches!(
        client.delete_mbox("archive"),
        Err(MboxClientError::Delete(_))
    ));

    let names: Vec<String> = client
        .list_mboxes()
        .unwrap()
        .into_iter()
        .map(|m| m.name.to_string())
        .collect();
    assert_eq!(names, ["INBOX", "lists/rustlang"]);

    let leftovers: Vec<_> = fs::read_dir(tmp.path())
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().contains("io-mbox-"))
        .collect();
    assert!(leftovers.is_empty(), "temporary files left behind");
    assert!(!tmp.path().join("spool.lock").exists());
}

#[test]
fn a_move_keeps_a_message_whose_sender_no_from_line_can_carry() {
    let tmp = tempdir().unwrap();
    let client = client(&tmp);
    client.create_mbox("target").unwrap();

    let source = format!(
        "From {} Mon Jan 1 0:00 24\nSubject: long\n\nbody\n",
        "a".repeat(4070)
    );
    fs::write(tmp.path().join("mail/source"), source).unwrap();
    let index = client.sync_index("source", None).unwrap().index;
    assert_eq!(index.entries.len(), 1);

    let entries = index.entries.clone();
    let moved = client
        .r#move("source", "target", entries, Some(index))
        .unwrap();
    assert_eq!(moved.entries.len(), 1);
    assert!(moved.index.entries.is_empty());

    let target = client.sync_index("target", None).unwrap().index;
    assert_eq!(target.entries, moved.entries);
    assert_eq!(target.entries[0].sender, "MAILER-DAEMON");
    let message = client.get("target", target.entries[0].clone()).unwrap();
    assert_eq!(message.contents, b"Subject: long\n\nbody\n");
}

#[test]
fn a_name_leaving_the_root_is_refused() {
    let tmp = tempdir().unwrap();
    let client = client(&tmp);
    client.create_mbox("inside").unwrap();
    let inside = tmp.path().join("mail/inside");
    let outside = tmp.path().join("notes");
    let notes = b"From a@b Mon Jan  1 00:00:00 2024\n\nx\n";
    fs::write(&outside, notes).unwrap();

    fn refused<T>(result: Result<T, MboxClientError>) -> bool {
        matches!(result, Err(MboxClientError::InvalidName(_)))
    }

    for name in [
        "../notes",
        "inside/../../notes",
        "./inside",
        "inside/.",
        "..",
        ".",
        "",
        "in\0side",
    ] {
        assert!(refused(client.delete_mbox(name)), "{name:?}");
        assert!(refused(client.rename_mbox(name, "renamed")), "{name:?}");
        assert!(refused(client.rename_mbox("inside", name)), "{name:?}");
        assert!(refused(client.create_mbox(name)), "{name:?}");
        assert!(refused(client.append(name, vec![item("x")])), "{name:?}");
        assert!(
            refused(client.update(name, BTreeMap::new(), None)),
            "{name:?}"
        );
        assert!(refused(client.copy("inside", name, Vec::new())), "{name:?}");
        assert!(refused(client.sync_index(name, None)), "{name:?}");
    }

    assert!(inside.exists());
    assert_eq!(fs::read(&outside).unwrap(), notes);
    let left: Vec<_> = fs::read_dir(tmp.path()).unwrap().collect();
    assert_eq!(left.len(), 2, "nothing created beside the root");
}

#[cfg(unix)]
#[test]
fn a_listing_walks_each_directory_once() {
    let tmp = tempdir().unwrap();
    let client = client(&tmp);
    let root = tmp.path().join("mail");
    client.create_mbox("a").unwrap();
    client.create_mbox("sub/b").unwrap();
    fs::create_dir(tmp.path().join("far")).unwrap();
    fs::write(tmp.path().join("far/c"), b"").unwrap();

    std::os::unix::fs::symlink(&root, root.join("sub/up")).unwrap();
    std::os::unix::fs::symlink(&root, root.join("again")).unwrap();
    std::os::unix::fs::symlink(root.join("sub"), root.join("twin")).unwrap();
    std::os::unix::fs::symlink(tmp.path().join("far"), root.join("far")).unwrap();
    std::os::unix::fs::symlink(root.join("a"), root.join("alias")).unwrap();

    let names: Vec<String> = client
        .list_mboxes()
        .unwrap()
        .into_iter()
        .map(|m| m.name.to_string())
        .collect();
    assert_eq!(names, ["a", "alias", "far/c", "sub/b"]);
}

#[test]
fn a_mailbox_cannot_be_named_after_a_side_file() {
    let tmp = tempdir().unwrap();
    let client = client(&tmp);
    client.create_mbox("x").unwrap();

    for name in ["x.lock", "x.msf", "x.lock/y", "y/x.msf"] {
        let created = client.create_mbox(name);
        assert!(
            matches!(created, Err(MboxClientError::InvalidName(_))),
            "{name}"
        );
        let appended = client.append(name, vec![item("x")]);
        assert!(
            matches!(appended, Err(MboxClientError::InvalidName(_))),
            "{name}"
        );
    }

    fs::create_dir(tmp.path().join("mail/dir.lock")).unwrap();
    fs::write(tmp.path().join("mail/dir.lock/hidden"), b"").unwrap();
    let names: Vec<String> = client
        .list_mboxes()
        .unwrap()
        .into_iter()
        .map(|m| m.name.to_string())
        .collect();
    assert_eq!(names, ["x"]);
}

#[test]
fn a_rewrite_leaves_no_temp_file_beside_the_mbox() {
    let tmp = tempdir().unwrap();
    let mut client = client(&tmp);
    client.temp_dir = None;
    client.create_mbox("box").unwrap();
    let appended = client
        .append("box", vec![item("one"), item("two")])
        .unwrap();

    let edits = BTreeMap::from([(appended[0].id.clone(), MboxEntryUpdateEdit::Remove)]);
    let out = client.update("box", edits, None).unwrap();
    assert_eq!(out.removed, 1);

    let left: Vec<String> = fs::read_dir(tmp.path().join("mail"))
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(left, ["box"]);
}

#[test]
fn busy_dotlock_blocks_writers() {
    let tmp = tempdir().unwrap();
    let mut client = client(&tmp);
    client.lock.timeout_secs = Some(0);
    fs::write(tmp.path().join("spool"), b"").unwrap();
    fs::write(tmp.path().join("spool.lock"), b"").unwrap();

    let err = client.append("INBOX", vec![item("x")]).unwrap_err();
    assert!(matches!(
        err,
        MboxClientError::EntryAppend(MboxEntryAppendError::Lock(
            MboxLockAcquireError::DotlockBusy(_)
        ))
    ));
    assert!(
        tmp.path().join("spool.lock").exists(),
        "another process's dotlock is left alone"
    );
    assert_eq!(fs::read(tmp.path().join("spool")).unwrap(), b"");

    let old = SystemTime::now() - Duration::from_secs(3600);
    let lock = OpenOptions::new()
        .write(true)
        .open(tmp.path().join("spool.lock"))
        .unwrap();
    lock.set_modified(old).unwrap();
    drop(lock);
    client.append("INBOX", vec![item("x")]).unwrap();
    assert!(!tmp.path().join("spool.lock").exists());
}

#[cfg(unix)]
#[test]
fn busy_fcntl_lock_blocks_writers() {
    use std::os::fd::AsRawFd;

    let tmp = tempdir().unwrap();
    let mut client = client(&tmp);
    client.lock = MboxLockOptions {
        skip_dotlock: true,
        timeout_secs: Some(0),
        ..Default::default()
    };
    fs::write(tmp.path().join("spool"), b"").unwrap();

    // NOTE: a classic POSIX lock, as an MTA takes it. Linux makes it
    // conflict with the client's OFD lock.
    let holder = OpenOptions::new()
        .read(true)
        .write(true)
        .open(tmp.path().join("spool"))
        .unwrap();
    let mut flock: libc::flock = unsafe { std::mem::zeroed() };
    flock.l_type = libc::F_WRLCK as _;
    flock.l_whence = libc::SEEK_SET as _;
    let ret = unsafe { libc::fcntl(holder.as_raw_fd(), libc::F_SETLK, &mut flock) };
    assert_eq!(ret, 0);

    let err = client.append("INBOX", vec![item("x")]).unwrap_err();
    assert!(matches!(
        err,
        MboxClientError::EntryAppend(MboxEntryAppendError::Lock(MboxLockAcquireError::FcntlBusy(
            _
        )))
    ));

    drop(holder);
    client.append("INBOX", vec![item("x")]).unwrap();
}

#[cfg(unix)]
#[test]
fn unwritable_spool_directory_names_the_option() {
    use std::os::unix::fs::PermissionsExt;

    let tmp = tempdir().unwrap();
    let client = client(&tmp);
    let spool_dir = tmp.path().join("var-mail");
    fs::create_dir(&spool_dir).unwrap();
    fs::write(spool_dir.join("me"), b"").unwrap();
    fs::set_permissions(&spool_dir, fs::Permissions::from_mode(0o555)).unwrap();

    let mut client = client;
    client.store.inbox = Some(spool_dir.join("me").into());

    let result = client.append("INBOX", vec![item("x")]);
    fs::set_permissions(&spool_dir, fs::Permissions::from_mode(0o755)).unwrap();

    // NOTE: root ignores directory permissions.
    if unsafe { libc::geteuid() } == 0 {
        return;
    }
    let err = result.unwrap_err();
    assert!(matches!(err, MboxClientError::DotlockDenied(_)));
    assert!(err.to_string().contains("skip dotlocking"));

    client.lock.skip_dotlock = true;
    client.append("INBOX", vec![item("x")]).unwrap();
}

/// A foreign MTA delivers while the client rewrites and removes: no
/// delivery is lost, no removed message comes back.
#[cfg(unix)]
#[test]
fn concurrent_delivery_is_never_lost() {
    use std::{os::fd::AsRawFd, thread};

    let tmp = tempdir().unwrap();
    let mut client = client(&tmp);
    client.lock.timeout_secs = Some(30);
    let spool = tmp.path().join("spool");
    fs::write(&spool, b"").unwrap();
    client
        .append(
            "INBOX",
            (0..20).map(|i| item(&format!("seed{i}"))).collect(),
        )
        .unwrap();

    let deliveries = 200;
    let mta = {
        let spool = spool.clone();
        thread::spawn(move || {
            let dotlock = spool.with_extension("lock");
            for i in 0..deliveries {
                // NOTE: procmail-style delivery: dotlock, then a classic
                // POSIX lock, then a plain append.
                while OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&dotlock)
                    .is_err()
                {
                    thread::sleep(Duration::from_millis(1));
                }
                let mut file = OpenOptions::new().append(true).open(&spool).unwrap();
                let mut flock: libc::flock = unsafe { std::mem::zeroed() };
                flock.l_type = libc::F_WRLCK as _;
                flock.l_whence = libc::SEEK_SET as _;
                assert_eq!(
                    unsafe { libc::fcntl(file.as_raw_fd(), libc::F_SETLKW, &mut flock) },
                    0
                );
                let message = format!(
                    "From mta@example.org Mon Jan  1 00:00:00 2024\nMessage-ID: <mta{i}@example.org>\nSubject: delivery {i}\n\nbody {i}\n\n"
                );
                file.write_all(message.as_bytes()).unwrap();
                file.sync_all().unwrap();
                drop(file);
                fs::remove_file(&dotlock).unwrap();
                thread::sleep(Duration::from_millis(1));
            }
        })
    };

    let mut removed = 0;
    let mut index = None;
    let mut racing = 0;
    let mut round = 0;
    while round < 10 || !mta.is_finished() {
        round += 1;
        if !mta.is_finished() {
            racing += 1;
        }
        let current = client.sync_index("INBOX", index.take()).unwrap().index;
        let mut edits = BTreeMap::new();
        for (i, entry) in current.entries.iter().enumerate() {
            if entry.sender == "alice@example.org" && i % 7 == round % 7 && removed < 10 {
                edits.insert(entry.id.clone(), MboxEntryUpdateEdit::Remove);
                removed += 1;
            } else if i % 3 == round % 3 {
                let mut flags = entry.flags.clone();
                let keyword = MboxKeyword::new(format!("r{round}")).unwrap();
                flags.insert(MboxFlag::Keyword(keyword));
                edits.insert(entry.id.clone(), MboxEntryUpdateEdit::Flags(flags));
            }
        }
        index = Some(client.update("INBOX", edits, Some(current)).unwrap().index);
    }
    mta.join().unwrap();
    assert!(racing >= 3, "the rewrites did not race the deliveries");

    let index = client.sync_index("INBOX", index).unwrap().index;
    assert_eq!(index, client.sync_index("INBOX", None).unwrap().index);
    assert_eq!(index.entries.len(), 20 - removed + deliveries);

    let bytes = fs::read(&spool).unwrap();
    let text = String::from_utf8_lossy(&bytes);
    for i in 0..deliveries {
        assert_eq!(
            text.matches(&format!("<mta{i}@example.org>")).count(),
            1,
            "delivery {i}"
        );
    }
}
