---
cairn: change
id: himalaya-live-test-fixes
status: landed
created: 2026-11-13
---

# Fixes from the Himalaya mbox live test campaign

## Why

The Himalaya mbox live test campaign (2026-11-13) found io-mbox at fault in five places: a `Content-Length` longer than its body made one id swallow the following messages (so a delete removed three); a huge lock timeout overflowed (a panic in debug, a random wait in release); any file below the root, a README or a symlink to /etc/passwd, listed as a mailbox and took appends; a flag write left `X-Mozilla-Status` stale, so Thunderbird and Himalaya disagreed; and a delete or rename left the `.msf` summary behind. A busy lock also could not be told from other failures without matching five nested error enums.

## What

- The scanner trusts a `Content-Length` only when its range ends on a message boundary, as mutt checks it, a shadow scan taking over when it does not.
- The lock wait count saturates.
- An append refuses a non-empty file that does not open on a `From_` line, and the client listing leaves such files out.
- A flag write keeps the read, replied, marked and expunged bits of an existing `X-Mozilla-Status` in step, in place.
- Delete and rename remove the source's `.msf`.
- `MboxClientError::lock_busy` tells a busy lock apart.
