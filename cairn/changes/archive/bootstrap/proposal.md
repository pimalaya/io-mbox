---
cairn: change
id: bootstrap
status: landed
created: 2026-09-30
---

# Bootstrap

## Why

Himalaya issue 697 asks to read the local spool, and full mbox support needs a library: the Rust crates that exist either only read (mboxshell, mail-parser's iterator) or leave flags and removal unimplemented (meli, GPL). io-mbox is the I/O-free mbox crate the Pimalaya stack lacks, on io-maildir's conventions.

## What

Streaming scan of the four variants with content ids, a resumable index, reads, appends, locked in-place rewrites of flags and removals, copies and moves, and the mailbox lifecycle, plus a std client with OFD fcntl locks, and a corpus suite running invariants on public archives.
