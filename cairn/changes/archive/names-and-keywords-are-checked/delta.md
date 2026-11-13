---
cairn: delta
change: names-and-keywords-are-checked
---

## ADDED Requirements

### Requirement: A name stays below the root

A mailbox name SHALL resolve to a path only when it has one segment at least, none of them `.` or `..`, and holds no NUL. Any other name SHALL be refused by every operation taking a name, before any filesystem request. `INBOX` SHALL still resolve to the spool wherever it lives.

### Requirement: A keyword reads back as written

A keyword SHALL be accepted only when it is not empty, is 255 bytes long at most, has no whitespace at either end and holds no ASCII whitespace, comma or control character, so no flag set can hold one an `X-Keywords` field would split, cut or let end the header. A token of `X-Keywords` or `X-Mozilla-Keys` that is no such keyword SHALL be left out when flags are read. A keyword SHALL be checked again when deserialized.

### Requirement: A listing walks each directory once

The driver SHALL report a symbolic link to a directory as such in a directory read, a link to a file counting as a file, and SHALL give the canonical path of a directory when asked. The listing SHALL track the canonical path of every directory it reaches, read the directories below the root first, then follow a link to a directory only when it leads to one not reached yet. A directory is so listed under its own name rather than a link's, and a link back into the tree is not followed.

#### Scenario: A link back to the root

- GIVEN a root holding a mailbox and, in a subdirectory, a link to the root itself
- WHEN the mailboxes are listed
- THEN the mailbox is listed once, under its own name

## MODIFIED Requirements

## REMOVED Requirements
