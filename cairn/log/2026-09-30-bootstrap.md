---
cairn: log
change: bootstrap
landed: 2026-09-30
---

# Bootstrap

io-mbox was created with five capabilities: format (From_ line check, the four variants, c-client and Thunderbird flag fields, the pseudo message), scan (streaming, boundaries, content ids, located flag fields), index (reuse, resume, rebuild, checked reads), write (locking, appends, in-place rewrites, copies and moves, mailbox lifecycle) and client (OFD locks, cleanup that keeps temporary files, the dotlock permission hint).

Three behaviours were settled by the corpus rather than planned. A missing final newline hashes as present, so the newline a flag rewrite must add keeps the id. A blank line ending the header opens the `Content-Length` range even while it is still pending. A read scans its range with a synthetic separator, so a blank line ending a message stays content.
