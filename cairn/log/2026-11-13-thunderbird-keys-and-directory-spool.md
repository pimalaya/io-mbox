---
cairn: log
change: thunderbird-keys-and-directory-spool
landed: 2026-11-13
---

# Thunderbird keys and a directory spool

Leftovers of the Himalaya mbox test campaign of 2026-11-13 (re-verification report VMB, findings N1 and N3).

- Scan, format and write: `X-Mozilla-Keys` is now located among the flag fields and entries record that they carry it (`MboxEntry::mozilla_keys`, serde default). A flag write cuts it with the c-client fields and writes it again after them from the keywords (`MboxFlags::to_mozilla_keys`), so a `flag set` dropping a label no longer leaves it for Thunderbird. The content id is unchanged, the field being volatile already.
- Client: `list_mboxes` leaves out a spool that is a directory, which no read opens.

Capabilities moved: scan, format, write, client.
