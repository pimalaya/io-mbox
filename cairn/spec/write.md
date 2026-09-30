---
cairn: spec
capability: write
status: current
---

# Write

How the crate changes an mbox file.

### Requirement: Every write is locked

Appending, rewriting and deleting SHALL first check the file exists, then take the dotlock (`<file>.lock`, created exclusively), then an exclusive fcntl lock on the file, retrying a busy lock every 100 ms up to a timeout (10 s by default), and SHALL release both in reverse order whatever the outcome. A dotlock older than the stale age (300 s by default) SHALL be broken. Each lock MAY be skipped by option. A lock that cannot be taken SHALL fail the write.

### Requirement: Appends pad and quote

An append SHALL store each message with LF line endings, converting CRLF ones, since a reader that meets a CRLF `From_` line takes it for body text. It SHALL pad the file so the previous message ends with a blank line, then write each message as a `From_` line (the given sender, else the `Return-Path` or `From` address, else `MAILER-DAEMON`, and the given or current date), its header with the flag fields replaced (and `Content-Length` for the mboxcl variants), its quoted body ended by a newline, and a blank separator line, in one positioned write followed by a sync.

### Requirement: Rewrites stay in place

Flag changes and removals SHALL be applied under lock, after a sync of the index, by writing the new layout of the file from the first edited message to a temporary file (unchanged ranges copied, flag fields swapped at the end of the header, removed messages skipped), then copying it back over the file at the same offset, truncating and syncing. The file SHALL keep its inode, owner and mode. An edit leaving a message's flags unchanged SHALL be skipped, and a rewrite with nothing to do SHALL NOT touch the file. The temporary file SHALL be removed once copied back, and SHALL be kept when the copy back fails.

#### Scenario: Flags set then restored

- GIVEN a file whose messages carry flag fields in any position
- WHEN flags are changed then set back to their original value
- THEN every byte outside the flag fields is unchanged, and every id too

### Requirement: Copies and moves

A copy SHALL read each message and append it to the target with its flags, sender and date. A move SHALL copy, then remove the messages from the source, never holding both files locked, so an interruption leaves the messages in both files rather than in neither.

### Requirement: Mailbox lifecycle

Creating a mailbox SHALL create an empty file and its parent directories, and fail when a file exists. Renaming SHALL refuse a missing source or an existing target. Deleting SHALL remove the file under lock. Listing SHALL walk the root recursively, skip hidden files and `.lock`, `.msf` and `.lck` files, map Thunderbird `.sbd` directories to child names when the store says so, and list the spool as `INBOX` when it exists.
