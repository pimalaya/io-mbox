---
cairn: change
id: writes-check-the-file
status: landed
created: 2026-11-13
---

# Appends and edits check what is on disk

## Why

The family audit of 2026-11-13 found two writes that trusted what they had computed over what the file holds (MBOX-02, MBOX-03).

- An append scanned its rendered bytes back but never compared the entries with the messages given. A `From_` line the scanner rejects was written and synced all the same, and the append reported success with fewer entries: the message became body text of the one before. Two inputs gave such a line: a sender without whitespace of 4,066 bytes or more (the line passes the 4 KiB the scanner keeps whole), taken from the message's own `Return-Path` or `From` field when the caller gives none, and a date whose year does not print on four digits. A move copies with the source's sender and date, then removes the source: a source line that fits with a short date form and no longer fits once rendered again left the message in neither file as a message.
- A flag write or a removal planned its cuts from the index alone. A sync resumes an index when the file only grew and reuses it when the stat matches, so a rewrite in place by another client (the four `X-Mozilla-Status` digits, flag fields changing length inside a header of the same size) is not seen: the edit started from flags the file no longer held, skipping a change equal to the stale ones, and cut flag fields at stale offsets, which can take bytes of the neighbouring fields.

## What

- An append fails before the write, nothing written, unless each message of the batch scans back as exactly one entry at its own `From_` line (`MboxEntryAppendError::Unreadable`, with the index of the message).
- `MboxFromLine::format` only writes lines that parse back and that the scanner reads whole: a sender too long for the line becomes `MAILER-DAEMON`, the year takes four digits, a date outside the years 0000 to 9999 is brought back inside.
- Unless its sync scanned the whole file, a rewrite scans each message to edit again from the file, separator included, takes flags and field places from that scan, and fails as stale before anything is written when the message is no longer there (`MboxEntryUpdateError::Stale`).
- The index spec stops claiming a resumed index equals a full scan in every case.
