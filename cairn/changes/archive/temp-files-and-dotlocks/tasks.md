---
cairn: tasks
change: temp-files-and-dotlocks
---

- [x] Refuse a name with a `.lock` or `.msf` segment, and skip such directories in the listing, with regression tests.
- [x] Create the temporary file beside the mbox, private, with a fallback, and keep it only once the copy back started, with regression tests.
- [x] Name the mbox and the offset in `MboxClientError::Interrupted`.
- [x] Refresh the dotlock from the update, and release only the dotlock this run created, with regression tests.
- [x] Find the entries to edit in one pass, with a test counting the entries visited.
- [ ] Padded flag fields patched in place: a format decision, see the proposal.
- [x] Fold into cairn/spec, write the log and the CHANGELOG.
