---
cairn: log
change: append-lf-line-endings
landed: 2026-09-30
---

# Appends store LF line endings

The write capability moved: an append now converts a message's CRLF line endings to LF. Found while wiring Himalaya's mbox backend: a CRLF message appended by `message add` got a CRLF `From_` line, and GNU `mail -H` then rewrote the file with that line quoted, merging two messages. The GNU mail interop test now appends a CRLF message.
