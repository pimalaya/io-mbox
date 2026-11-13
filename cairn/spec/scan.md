---
cairn: spec
capability: scan
status: current
---

# Scan

How a file is split into entries.

### Requirement: Scanning is streaming

The scanner SHALL accept the file in chunks of any size and produce the same entries whatever the chunking. It SHALL hold at most one line of 4 KiB, one header capped at 256 KiB and 64 KiB of volatile fields, so memory stays bounded whatever the file size. A longer line is streamed and can open neither a message nor a separator. The bounds hold for each of the five scans a file can nest. Entries completed inside an unchecked `Content-Length` range are held until the range is checked.

### Requirement: Message boundaries

A message SHALL span from the end of its `From_` line to the blank line preceding the next `From_` line, that blank line excluded. At end of file, one trailing blank line is the separator. Bytes before the first `From_` line are ignored.

### Requirement: Content-Length checks are bounded

While the body range of a trusted `Content-Length` is unchecked, the scanner SHALL keep a second scan of that range, from its first `From_` line on, that ignores the length, and SHALL hand over to it when the range ends off a message boundary. A message of that second scan may open a range of its own. Unchecked ranges SHALL nest four deep at most, a `Content-Length` declared inside four of them being ignored, so a line is scanned five times at most whatever the file nests. A range proved right or wrong SHALL no longer count. When nested ranges are proved wrong at the same line, each of them SHALL be ignored and no message SHALL be reported twice.

#### Scenario: Thousands of nested lengths

- GIVEN a message whose body repeats a `From_` line, a `Content-Length` and a blank line 4,000 times
- WHEN the file is scanned trusting `Content-Length`
- THEN at most four scans run beside the main one, and the scan fits a 256 KiB stack

### Requirement: Content ids

Each message SHALL be identified by the SHA-256 of its bytes, the volatile fields (`Status`, `X-Status`, `X-Keywords`, `X-UID`, `Content-Length`, `X-IMAP`, `X-IMAPbase`, `X-Mozilla-Status`, `X-Mozilla-Status2`, `X-Mozilla-Keys`, continuations included) left out and a missing final newline counted as present, truncated to 80 bits and base32-encoded. The n-th copy of a byte-identical message, in file order, SHALL carry the suffix `-n` from the second on.

#### Scenario: A flag rewrite by another client

- GIVEN a message whose `Status` field another client rewrote
- WHEN the file is scanned again
- THEN the message keeps its id

### Requirement: Flag fields are located

Each entry SHALL record where its `Status`, `X-Status`, `X-Keywords` and `X-Mozilla-Keys` fields sit, whether it carries an `X-Mozilla-Keys`, and where the four hex digits of an `X-Mozilla-Status` field sit with the bits they hold, so a rewrite can swap them without parsing the message.
