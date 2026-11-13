---
cairn: log
change: bounded-content-length-nesting
landed: 2026-11-13
---

# Bounded Content-Length nesting and length arithmetic

Follow-up of the family audit of 2026-11-13 (MBOX-01, MBOX-07, MBOX-09). Capabilities moved: scan, format, index.

- Scan and format: a scan now carries how many unchecked `Content-Length` ranges enclose it and ignores the field once four do. Before, a shadow scan trusted lengths like the main one and opened its own shadow, one level for each `From_` line, `Content-Length` and blank line a body repeated: every line went down the chain by recursion, every level copied the id counts, and 4,000 levels overflowed a 2 MiB stack. A range proved wrong frees its level, so wrong lengths in a row cost nothing later. Ids are numbered when entries leave the scanner, which gives the same ids as before without a copy of the counts per shadow.
- Scan: a shadow taking over now checks its own range at the line it takes over on. Two nested ranges proved wrong at the same line used to leave a stale shadow behind, and the next wrong length reported a message twice with overlapping entries. Found by comparing the scanner with its previous version on generated files; the audit had not listed it. With that fix applied to both, the two versions agree on each of 200,000 generated files holding four lengths or fewer.
- Format: a `Content-Length` whose range would end past the largest offset is ignored (`18446744073709551615` panicked where overflow checks are on).
- Index: a sync clamps what is left of the file to the read size before narrowing it, where a 32-bit target with a multiple of 4 GiB left asked for zero bytes and returned a cut index. The test passes on a 64-bit host with or without the fix.
- Fuzzing: fuzz/seeds/scan holds inputs opening on an options byte and fuzz/scan.dict the tokens, since the fixtures copied as they were lost their first `From_` line to that byte and never turned `Content-Length` on. A 60 second run (310,498 inputs) found nothing. fuzz/Cargo.lock follows the crate's sha2 0.11.
