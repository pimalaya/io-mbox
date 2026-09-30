---
cairn: spec
capability: scan
status: current
---

# Scan

How a file is split into entries.

### Requirement: Scanning is streaming

The scanner SHALL accept the file in chunks of any size and produce the same entries whatever the chunking. It SHALL hold at most one line of 4 KiB, one header capped at 256 KiB and 64 KiB of volatile fields, so memory stays bounded whatever the file size. A longer line is streamed and can open neither a message nor a separator.

### Requirement: Message boundaries

A message SHALL span from the end of its `From_` line to the blank line preceding the next `From_` line, that blank line excluded. At end of file, one trailing blank line is the separator. Bytes before the first `From_` line are ignored.

### Requirement: Content ids

Each message SHALL be identified by the SHA-256 of its bytes, the volatile fields (`Status`, `X-Status`, `X-Keywords`, `X-UID`, `Content-Length`, `X-IMAP`, `X-IMAPbase`, `X-Mozilla-Status`, `X-Mozilla-Status2`, `X-Mozilla-Keys`, continuations included) left out and a missing final newline counted as present, truncated to 80 bits and base32-encoded. The n-th copy of a byte-identical message, in file order, SHALL carry the suffix `-n` from the second on.

#### Scenario: A flag rewrite by another client

- GIVEN a message whose `Status` field another client rewrote
- WHEN the file is scanned again
- THEN the message keeps its id

### Requirement: Flag fields are located

Each entry SHALL record where its `Status`, `X-Status` and `X-Keywords` fields sit, so a rewrite can swap them without parsing the message.
