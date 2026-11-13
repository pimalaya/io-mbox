---
cairn: log
change: himalaya-live-test-fixes
landed: 2026-11-13
---

# Fixes from the Himalaya mbox live test campaign

The scan, format, write and client capabilities moved, after the Himalaya mbox live test campaign of 2026-11-13 (report MB, findings F3, F4, F5, F7 and F8).

- Scan and format: a `Content-Length` is trusted only when its body range ends on a blank line then a `From_` line, or at the end of the file, as mutt checks it. The scanner never looks ahead, so from the first `From_` line a length keeps from splitting, a shadow scanner reads on as if the length were absent, and takes over when the range ends anywhere else. A length past the end of the file or into a later message no longer merges the following messages under one id. One existing test declared a length counting the separator, which mutt rejects too; it now declares the body length.
- Write: the lock wait count saturates (`timeout_secs` of 99999999999999999 panicked in debug builds). An append refuses a non-empty file that does not open on a `From_` line. A flag write sets the read, replied, marked and expunged bits of an existing `X-Mozilla-Status` in place, which entries now locate (`MboxEntry::mozilla_status`, serde default). Delete and rename remove the source's `.msf`.
- Client: `list_mboxes` leaves out files that are no mbox, reading their first KiB; `MboxClientError::lock_busy` tells a busy dotlock or fcntl lock apart.

`MboxEntry` gained a public field and `MboxEntryAppendError` a variant, both breaking for exhaustive constructions and matches: the next release is 0.2.0.
