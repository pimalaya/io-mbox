---
cairn: spec
capability: write
status: current
---

# Write

How the crate changes an mbox file.

### Requirement: Every write is locked

Appending, rewriting and deleting SHALL first check the file exists, then take the dotlock (`<file>.lock`, created exclusively), then an exclusive fcntl lock on the file, retrying a busy lock every 100 ms up to a timeout (10 s by default, one too large to count in milliseconds waiting as long as one that can), and SHALL release both in reverse order whatever the outcome. A dotlock older than the stale age (300 s by default) SHALL be broken. Each lock MAY be skipped by option. A lock that cannot be taken SHALL fail the write.

### Requirement: Appends pad and quote

An append to a non-empty file that does not open on a `From_` line, leading blank lines aside, SHALL be refused before any write, so a text file or a binary is never appended to. An append SHALL store each message with LF line endings, converting CRLF ones, since a reader that meets a CRLF `From_` line takes it for body text. It SHALL pad the file so the previous message ends with a blank line, then write each message as a `From_` line (the given sender, else the `Return-Path` or `From` address, else `MAILER-DAEMON`, and the given or current date), its header with the flag fields replaced (and `Content-Length` for the mboxcl variants), its quoted body ended by a newline, and a blank separator line, in one positioned write followed by a sync. Before the write, the batch SHALL be scanned back with the scanner options given, and the append SHALL fail, nothing written, unless each message reads back as exactly one entry opening on its own `From_` line.

### Requirement: Rewrites stay in place

Flag changes and removals SHALL be applied under lock, after a sync of the index, by writing the new layout of the file from the first edited message to a temporary file (unchanged ranges copied, flag fields swapped at the end of the header, the digits of an `X-Mozilla-Status` swapped in place, an `X-Mozilla-Keys` field moved among the flag fields and written from the keywords, removed messages skipped), then copying it back over the file at the same offset, truncating and syncing. The file SHALL keep its inode, owner and mode. An edit leaving a message's flags unchanged SHALL be skipped, and a rewrite with nothing to do SHALL NOT touch the file. The temporary file SHALL be removed once copied back, and SHALL be kept when the copy back fails. Unless the sync scanned the whole file, each message to edit SHALL be scanned again from the file, separator included. The flags an edit is compared with and the places of the flag fields SHALL come from that scan. When the file no longer holds the same message there (content id, length and offset), the rewrite SHALL fail as stale, nothing written, and a rewrite given no index SHALL go through.

#### Scenario: Another client marks a message read in place

- GIVEN an index, a message whose `X-Mozilla-Status` digits another client then flipped in place, and mail delivered since
- WHEN the message is set back to unread with that index
- THEN the digits are written again from the flags asked for, the edit being compared with the flags the file holds

#### Scenario: Flags set then restored

- GIVEN a file whose messages carry flag fields in any position
- WHEN flags are changed then set back to their original value
- THEN every byte outside the flag fields is unchanged, and every id too

### Requirement: Copies and moves

A copy SHALL read each message and append it to the target with its flags, sender and date. A move SHALL copy, then remove the messages from the source, never holding both files locked, so an interruption leaves the messages in both files rather than in neither.

### Requirement: Mailbox lifecycle

Creating a mailbox SHALL create an empty file and its parent directories, and fail when a file exists. Renaming SHALL refuse a missing source or an existing target. Deleting SHALL remove the file under lock. Renaming and deleting SHALL remove the source's Thunderbird summary (`<file>.msf`), which Thunderbird rebuilds. Listing SHALL walk the root recursively, skip hidden files and `.lock`, `.msf` and `.lck` files, map Thunderbird `.sbd` directories to child names when the store says so, and list the spool as `INBOX` when it exists.

### Requirement: A name stays below the root

A mailbox name SHALL resolve to a path only when it has one segment at least, none of them `.` or `..`, and holds no NUL. Any other name SHALL be refused by every operation taking a name, before any filesystem request. `INBOX` SHALL still resolve to the spool wherever it lives.

### Requirement: A listing walks each directory once

The driver SHALL report a symbolic link to a directory as such in a directory read, a link to a file counting as a file, and SHALL give the canonical path of a directory when asked. The listing SHALL track the canonical path of every directory it reaches, read the directories below the root first, then follow a link to a directory only when it leads to one not reached yet. A directory is so listed under its own name rather than a link's, and a link back into the tree is not followed.

#### Scenario: A link back to the root

- GIVEN a root holding a mailbox and, in a subdirectory, a link to the root itself
- WHEN the mailboxes are listed
- THEN the mailbox is listed once, under its own name
