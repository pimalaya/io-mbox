---
cairn: log
change: writes-check-the-file
landed: 2026-11-13
---

# Appends and edits check what is on disk

Follow-up of the family audit of 2026-11-13 (MBOX-02, MBOX-03). Capabilities moved: write, format, index.

- Write: an append compares the scan of its rendered batch with the messages given, and fails with `MboxEntryAppendError::Unreadable` before the positioned write unless each one reads back as exactly one entry at its own `From_` line. The check runs on the whole batch with the caller's scanner options, since that is what a reader will see: an mboxcl2 append read back without trusting `Content-Length` is refused when a body holds a `From_` line.
- Format: `MboxFromLine::format` replaces a sender longer than 4,065 bytes by `MAILER-DAEMON`, the bound being what is left of the scanner's 4,096 byte line once `From ` and the date are written, so every sender a line can carry is kept. The audit suggested a few hundred bytes; the exact bound was chosen so a copy keeps a long sender that reads back. The year is padded to four digits, which round-trips the years 0 to 999, and a date outside 0000 to 9999 is clamped.
- Write: the move is unchanged. A copy appends one message at a time, so an append that now fails stops the move before the removal, and a test moves a message whose 4,070 byte sender fits its source line and not a rendered one: it lands in the target under `MAILER-DAEMON`.
- Write and index: `MboxEntryUpdate` scans each message to edit again when its sync resumed or reused the index, reading from the entry to the next one in chunks, and swaps the index entry for the fresh one, its numbered id kept. A different content id, length or offset, a short read or a separator that is not one blank line fails with `MboxEntryUpdateError::Stale`. An index rebuilt by the sync, under the same lock, is not scanned twice, so retrying without an index always goes through. Three tests: digits flipped in place then growth (the edit back to unread was skipped), flag fields changing length inside a header of the same size (the cut took the `S` of `Subject`), and a rewritten body (the removal went through).
- Index: the spec said a resumed index equals a full scan in every case. It does not after a rewrite in place before the last entry, and now says so. The entries a rewrite does not edit still carry what the index held.

`MboxEntryAppendError` and `MboxEntryUpdateError` each gained a variant, breaking for exhaustive matches. A caller holding an index should retry an update without it on `Stale`.
