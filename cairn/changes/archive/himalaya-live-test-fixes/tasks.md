---
cairn: tasks
change: himalaya-live-test-fixes
---

- [x] Validate `Content-Length` ranges in the scanner, with regression tests.
- [x] Saturate the lock wait count.
- [x] Refuse appends to, and list out, files that are no mbox.
- [x] Keep `X-Mozilla-Status` in step on flag writes.
- [x] Remove the `.msf` summary on delete and rename.
- [x] Add `MboxClientError::lock_busy`.
- [x] Fold into cairn/spec, write the log and the CHANGELOG.
