---
cairn: change
id: temp-files-and-dotlocks
status: landed
created: 2026-11-13
---

# Rewrites, temporary files and dotlocks

## Why

Three findings of the family audit of 2026-11-13 (MBOX-05, MBOX-11, MBOX-06) and one point left open by the name guards of the same day.

- A mailbox named `x.lock` resolved, and is the dotlock of the mailbox `x`: a writer to `x` took it for a lock left behind, removed it once it looked stale and put its own in its place. `x.msf` is the summary a delete or a rename of `x` removes.
- The temporary file of a rewrite went to the system temporary directory with the default mode. It holds the mailbox from the first edited message on: readable by every local user with a umask of 022, in RAM where that directory is a tmpfs, and gone after a power cut, which is when a half copied back mailbox needs it. A failed run kept it whether the copy back had started or not, and its error did not say which mailbox and offset it belonged to.
- A dotlock was never refreshed, so a rewrite longer than the stale age had its lock broken by the next writer. The release then removed whatever sat at the dotlock path, the other writer's lock included, or failed an otherwise successful update when the file was gone.
- Finding the entries to edit searched the index once per edit: 500,500 entries visited for 1,000 edits in a file of 1,000 messages.

## What

- A name with a segment ending in `.lock` or `.msf` does not resolve, and the listing does not walk a directory named so.
- `WantsTempFile` names the mbox and the offset. The std client creates the file beside the mbox, hidden, mode 0600 on Unix, falling back to the system directory where the directory of the mbox cannot be written to. A failed run removes it unless the mbox was written since, and `Interrupted` then names the mbox and the offset.
- A dotlock holds the pid and is known by it and by its inode. The update asks for a refresh every 256 reads (`WantsDotlockRefresh`); refresh, release and abort leave alone a dotlock that is no longer the one this run created, and a release of a dotlock already gone succeeds.
- The entries to edit are found in one pass over the index.

## Not done

The audit also suggests writing the flag fields with reserved padding and patching them in place when the new value fits, which would spare the two copies of the tail on most flag changes. It changes what the crate writes: a message whose flags are written would carry `Status`, `X-Status` and `X-Keywords` even when empty, padded with spaces, and keep them where they are instead of moving them to the end of the header; setting flags then restoring them would no longer give the original bytes back, which the write spec promises today. That is a format decision, left to the maintainer. Rendering an append by streaming pieces instead of concatenating them is not done either.
