---
cairn: change
id: names-and-keywords-are-checked
status: landed
created: 2026-11-13
---

# Mailbox names and keywords are checked

## Why

The family audit of 2026-11-13 found three places where a string from the caller reached the filesystem or the file unchecked (MBOX-08, MBOX-04).

- `MboxStore::resolve` kept `.` and `..` segments, so `delete_mbox("../../notes")` resolved outside the root, locked the file there and removed it; create, rename, append and update took the same names.
- The listing told a directory by a stat that follows links and queued every directory it met. One link back into the tree was walked until path resolution gave up, each level listing the same mailboxes under a longer name; two such links doubled the queue at every level.
- A keyword went as written into `X-Keywords` and `X-Mozilla-Keys`. One holding a line feed ended the header early, and with a blank line and a `From_` line after it a flag write added a forged message to the file. One holding a space or a comma read back as two keywords.

## What

- A name that is empty, or holds a `.` or `..` segment or a NUL, does not resolve: `MboxStore::resolve` returns an `Option`, `MboxPath::names_mbox` tells why, and every client helper fails with `MboxClientError::InvalidName`.
- The listing walks each directory once. The directory read reports a link to a directory as such (`MboxFileKind::DirLink`), the coroutine asks for canonical paths (`MboxYield::WantsDirCanonical`) and follows a link last, only to a directory it has not reached.
- A keyword is a checked type, `MboxKeyword`, which `MboxFlag::Keyword` now holds: not empty, 255 bytes at most, no whitespace at either end, no ASCII whitespace, comma or control character. A token of a field that is no keyword is left out when flags are read, and a keyword is checked again when deserialized.
