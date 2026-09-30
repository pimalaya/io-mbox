---
cairn: change
id: append-lf-line-endings
status: landed
created: 2026-09-30
---

# Appends store LF line endings

## Why

An append wrote the `From_` line, the header fields it adds and the separator with the line ending of the message, so a CRLF message (what Himalaya's `message add` hands over) landed with a CRLF `From_` line. io-mbox reads that back, but GNU mailutils does not take it for a separator: opening the file, even to list headers, rewrites it with the line quoted as `>From`, merging the message into the previous one.

## What

An append converts the CRLF line endings of a message to LF before writing it, as MTAs do when they deliver to an mbox. Reading CRLF files is unchanged.
