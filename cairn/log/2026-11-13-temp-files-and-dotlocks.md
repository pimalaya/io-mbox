---
cairn: log
change: temp-files-and-dotlocks
landed: 2026-11-13
---

# Rewrites, temporary files and dotlocks

Follow-up of the family audit of 2026-11-13 (MBOX-05, MBOX-11, MBOX-06) and of the point the name guards left open. Capabilities moved: write, client.

- Names: a segment ending in `.lock` or `.msf` no longer resolves (`MboxStore::resolve`, hence `MboxClientError::InvalidName`), and the listing skips a directory named so, so every listed name still resolves. A mailbox `x.lock`, aged one hour, was removed by an append to `x` with a stale age of 60 seconds. `.lck` files and hidden files stay as they were: left out of the listing, not refused.
- Temporary file: `MboxYield::WantsTempFile` carries the mbox path and the offset of the rewritten tail. The std client creates `.io-mbox-<pid>-<nanos>-<count>` beside the mbox with mode 0600 (it was 0644 in the system directory), and falls back to the system directory on a permission error, the case of a spool directory. The name is hidden so the listing does not take it for a mailbox, and does not carry the mbox name, which can be as long as a file name may be. `MboxClient::temp_dir` still overrides the place.
- Failed runs: the client now tells a temporary file being copied back from one that was only being filled, by whether the mbox was written or truncated since it was created. The first is kept and `MboxClientError::Interrupted` names it with the mbox and the offset. The second is removed: the mbox is intact, and an error telling to copy a half filled file over it would do harm. Before, every temporary file was kept. No sidecar is written, a later run does not finish a copy back by itself.
- Dotlocks: the driver records the inode of a dotlock it creates and what it wrote in it (the pid, or nothing when the write failed, as on a full disk). `WantsDotlockRefresh`, new, sets its modification time; `MboxEntryUpdate` yields it every 256 file reads while it holds a dotlock, holding the read back until the driver answers, which covers the scan, the check of edited entries, the copy to the temporary file and the copy back. Refresh, release and the cleanup of a failed run act only on the file this run created; a release of a lock taken in an earlier run goes by the pid alone. The count is in reads, not bytes: a caller setting a read size of several MiB gets fewer refreshes.
- Planning: `locate` moves the edits out of the map while it walks the ids once, and stops when none is left. The old loop, counted with a temporary counter, visited 500,500 entries for 1,000 edits; the test passes an iterator that counts and expects 1,000.

Not done, a format decision: flag fields written with padding and patched in place, see the proposal of this change for what it would change on disk. Left open: two writers that both find the same stale dotlock can still each break it in turn, the second removing the first one's fresh lock; the fcntl lock keeps them apart unless it is skipped.

Breaking: `MboxYield::WantsTempFile` gained fields, `MboxYield` and `MboxReply` a variant each (`WantsDotlockRefresh`, `DotlockRefresh`), `MboxClientError::Interrupted` two fields. A driver has to answer the refresh.
