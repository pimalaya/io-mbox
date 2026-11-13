---
cairn: delta
change: writes-check-the-file
---

## ADDED Requirements

### Requirement: A written From_ line reads back

A `From_` line the crate writes SHALL parse back to the same sender and date and SHALL fit the 4 KiB line the scanner keeps whole. A sender that is empty, holds whitespace or is longer than 4,065 bytes SHALL be replaced by `MAILER-DAEMON`. The year SHALL be written on four digits, and a date outside the years 0000 to 9999 SHALL be brought back to the nearest second inside.

## MODIFIED Requirements

### Requirement: Appends pad and quote

Before the write, the batch SHALL be scanned back with the scanner options given, and the append SHALL fail, nothing written, unless each message reads back as exactly one entry opening on its own `From_` line.

### Requirement: Rewrites stay in place

Unless the sync scanned the whole file, each message to edit SHALL be scanned again from the file, separator included. The flags an edit is compared with and the places of the flag fields SHALL come from that scan. When the file no longer holds the same message there (content id, length and offset), the rewrite SHALL fail as stale, nothing written, and a rewrite given no index SHALL go through.

### Requirement: Sync scans only what changed

The result SHALL equal a full scan as long as nothing before the last entry of the index was rewritten in place. Such a rewrite followed by growth is not seen: the entries before keep the flags and field places the index held, which is why reads and rewrites check the entries they touch.

## REMOVED Requirements
