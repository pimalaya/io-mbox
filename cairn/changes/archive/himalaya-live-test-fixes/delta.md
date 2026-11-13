---
cairn: delta
change: himalaya-live-test-fixes
---

## ADDED Requirements

### Requirement: A busy lock is told apart

The client error SHALL tell a lock another process held past the timeout, whichever write wanted it, from any other failure, so a caller can offer a retry.

### Requirement: Listing leaves out files that are no mbox

The client's listing SHALL leave out a file below the root that does not open on a `From_` line, leading blank lines aside, as mutt refuses such a file. An empty file, a file it cannot read and the spool SHALL stay listed.

## MODIFIED Requirements

### Requirement: Four variants

A `Content-Length` field SHALL be trusted only when the reader is told the file is an mboxcl variant, and only when the body range it declares ends on a message boundary: a blank line then a `From_` line, or the end of the file. A length off the boundary SHALL be ignored, the message split where it would be without it.

### Requirement: Flags live in c-client fields

A flag write on a message carrying an `X-Mozilla-Status` of four hex digits SHALL also set its read, replied, marked and expunged bits from the flags, in place, its other bits kept. No Thunderbird field is ever added.

### Requirement: Flag fields are located

Each entry SHALL also record where the four hex digits of an `X-Mozilla-Status` field sit, with the bits they hold.

### Requirement: Every write is locked

A timeout too large to count in milliseconds SHALL wait as long as one that can.

### Requirement: Appends pad and quote

An append to a non-empty file that does not open on a `From_` line, leading blank lines aside, SHALL be refused before any write.

### Requirement: Rewrites stay in place

The digits of an `X-Mozilla-Status` SHALL be swapped in place.

### Requirement: Mailbox lifecycle

Renaming and deleting SHALL remove the source's Thunderbird summary (`<file>.msf`).

## REMOVED Requirements
