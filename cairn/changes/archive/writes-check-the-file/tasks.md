---
cairn: tasks
change: writes-check-the-file
---

- [x] Fail an append before the write unless each message scans back as one entry, with regression tests.
- [x] Keep `MboxFromLine::format` to lines that parse back: sender length, four-digit year, date range.
- [x] Cover the move of a message whose sender no rendered line can carry.
- [x] Scan each edited entry again in `MboxEntryUpdate` when the index was resumed or reused, with regression tests for stale flags, stale field places and a moved message.
- [x] Fold into cairn/spec, write the log and the CHANGELOG.
