//! Interop with the MUAs sharing mbox files: a file io-mbox appended to,
//! flagged and pruned must read the same in formail (procmail) and in
//! GNU mailutils (`mail` and `messages`). Ignored by default, both must
//! be on the PATH:
//!
//! ```sh
//! nix shell nixpkgs#procmail nixpkgs#mailutils -c cargo test --test interop -- --ignored
//! ```

use std::{collections::BTreeMap, fs, process::Command};

use io_mbox::{
    client::MboxClient,
    entry::{append::MboxEntryAppendItem, update::MboxEntryUpdateEdit},
    flag::{MboxFlag, MboxFlags},
};
use tempfile::tempdir;

#[test]
#[ignore = "needs formail and GNU mail on the PATH"]
fn formail_and_mail_read_what_io_mbox_writes() {
    let tmp = tempdir().unwrap();
    let mut client = MboxClient::new(tmp.path());
    client.temp_dir = Some(tmp.path().to_path_buf());
    fs::write(tmp.path().join("box"), b"").unwrap();

    let items = (1..=4)
        .map(|i| MboxEntryAppendItem {
            contents: format!("From: Alice <alice@example.org>\nTo: bob@example.org\nSubject: message {i}\nMessage-ID: <{i}@example.org>\n\nFrom the start of line {i}\n>From quoted\n").into_bytes(),
            ..Default::default()
        })
        .collect();
    let entries = client.append("box", items).unwrap();

    let edits = BTreeMap::from([
        (entries[0].id.clone(), MboxEntryUpdateEdit::Remove),
        (
            entries[1].id.clone(),
            MboxEntryUpdateEdit::Flags(MboxFlags::from_iter([
                MboxFlag::Seen,
                MboxFlag::Old,
                MboxFlag::Flagged,
            ])),
        ),
        (
            entries[2].id.clone(),
            MboxEntryUpdateEdit::Flags(MboxFlags::from_iter([MboxFlag::Old])),
        ),
    ]);
    client.update("box", edits, None).unwrap();
    let path = tmp.path().join("box");

    let out = Command::new("sh")
        .arg("-c")
        .arg(format!(
            "formail -s sh -c 'cat >/dev/null; echo' < {}",
            path.display()
        ))
        .output()
        .expect("formail");
    let count = String::from_utf8_lossy(&out.stdout).lines().count();
    assert_eq!(count, 3, "formail message count");

    let out = Command::new("messages")
        .arg(&path)
        .output()
        .expect("messages");
    assert!(
        String::from_utf8_lossy(&out.stdout)
            .trim_end()
            .ends_with(": 3"),
        "messages count"
    );

    let out = Command::new("mail")
        .args(["-N", "-H", "-f"])
        .arg(&path)
        .output()
        .expect("mail");
    let summary = String::from_utf8_lossy(&out.stdout);
    println!("{summary}");
    let lines: Vec<&str> = summary.lines().collect();
    assert_eq!(lines.len(), 3, "mail message count");
    let status = |line: &str| line.chars().nth(1).unwrap_or(' ');
    assert!(
        lines[0].ends_with("message 2") && status(lines[0]) == ' ',
        "message 2 is read: {}",
        lines[0]
    );
    assert!(
        lines[1].ends_with("message 3") && status(lines[1]) == 'U',
        "message 3 is old unread: {}",
        lines[1]
    );
    assert!(
        lines[2].ends_with("message 4") && status(lines[2]) == 'N',
        "message 4 is new: {}",
        lines[2]
    );

    let out = Command::new("sh")
        .arg("-c")
        .arg(format!("echo 'print 1' | mail -N -f {}", path.display()))
        .output()
        .expect("mail print");
    let printed = String::from_utf8_lossy(&out.stdout);
    assert!(
        printed.contains("From the start of line 2"),
        "unquoted body: {printed}"
    );
    assert!(printed.contains("\n>From quoted"), "quoted body: {printed}");
}
