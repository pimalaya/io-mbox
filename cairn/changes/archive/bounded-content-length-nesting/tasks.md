---
cairn: tasks
change: bounded-content-length-nesting
---

- [x] Bound the nesting of unchecked `Content-Length` ranges, with regression tests on the number of scans and on a 256 KiB stack.
- [x] Number duplicate ids when entries leave the scanner instead of copying the counts into each shadow.
- [x] Ignore a `Content-Length` whose range would end past the largest offset.
- [x] Let a shadow taking over check its own range at the same line.
- [x] Clamp the sync read length before narrowing it.
- [x] Add fuzz seeds and a dictionary, and fix the seeding recipe in CONTRIBUTING.md.
- [x] Fold into cairn/spec, write the log and the CHANGELOG.
