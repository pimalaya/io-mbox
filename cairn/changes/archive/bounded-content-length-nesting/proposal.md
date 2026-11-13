---
cairn: change
id: bounded-content-length-nesting
status: landed
created: 2026-11-13
---

# Bounded Content-Length nesting and length arithmetic

## Why

The family audit of 2026-11-13 found three defects in how lengths are handled (MBOX-01, MBOX-07, MBOX-09), and checking the first one turned up a fourth.

- The shadow scan that reads the range of an unchecked `Content-Length` trusted lengths itself, so it opened a shadow of its own, and so on without limit: one more level for every `From_` line, `Content-Length` and blank line a body repeats. Every later line went down the whole chain by recursion and every level copied the id counts of the mailbox. One crafted message cost time and memory growing with the square of its size, and 4,000 levels (262 KB) overflowed a 2 MiB stack. Reachable only when the reader trusts `Content-Length`.
- `Content-Length: 18446744073709551615` overflowed the addition giving the end of the range: a panic where overflow checks are on, a wrapped offset elsewhere.
- A sync narrowed what is left of the file to the word size before clamping it to the read size. On a 32-bit target, with a multiple of 4 GiB left, it asked for zero bytes, took the empty read for the end of the file and returned an index lacking the rest.
- When two nested ranges were proved wrong at the same line, the shadow taking over skipped its own check of that line and kept a shadow of a message it had already closed. The next wrong length handed over to that stale shadow, which reported a message twice, the entries overlapping.

## What

- A scan carries how many unchecked ranges enclose it and ignores `Content-Length` once four do, so at most four shadows run beside the main scan and a line is scanned five times at most. A range proved wrong frees its level.
- Shadows no longer count ids: entries are numbered when they leave the scanner, in file order.
- A length whose range would end past the largest offset is ignored.
- A shadow taking over checks its own range at the line it takes over on.
- The sync clamps before it narrows.
- The fuzz target gets seeds opening on an options byte and a dictionary, so it reaches the `Content-Length` paths.
