---
cairn: log
change: names-and-keywords-are-checked
landed: 2026-11-13
---

# Mailbox names and keywords are checked

Follow-up of the family audit of 2026-11-13 (MBOX-08, MBOX-04). Capabilities moved: write, format, client.

- Names: `MboxStore::resolve` returns `None` for a name `MboxPath::names_mbox` rejects (no segment, a `.` or `..` segment, a NUL), and the client helpers fail with `MboxClientError::InvalidName` before any filesystem request. `delete_mbox("../notes")` used to remove a file beside the root. The rule is the one io-maildir landed the same day for its folders, plus the NUL. A caller that opens any path on purpose, as `mail -f` does, builds the `MboxFsPath` itself and does not go through the store.
- Listing: the walk now happens on canonical paths the coroutine keeps. The directory read reports `MboxFileKind::DirLink` for a link to a directory, `MboxList` first asks the canonical path of the root (`MboxYield::WantsDirCanonical`, the path as given standing in when it does not resolve), derives the one of each real directory from its parent, and resolves the links once no directory is left to read, following one only to a directory not reached yet. With one link back to the root the old walk listed every mailbox again at each level, under `sub/up/a`, `sub/up/sub/up/a` and so on, until path resolution failed; two links would double the queue at each level, which was not run. Links to directories outside the tree are still followed, as before, and listed under the link's name.
- Keywords: `MboxFlag::Keyword` holds an `MboxKeyword`, built by `MboxKeyword::new`, which refuses what an `X-Keywords` field would not give back. A flag write with the keyword `x`, two line feeds and a `From_` line added a fourth message to a file of three; `a b` read back as `a` and `b`. The rule is io-maildir's (not empty, not padded with whitespace, no control character) plus what the field separates on (ASCII whitespace and the comma) and a cap of 255 bytes, the length RFC 8621 gives a keyword. Reading leaves out a token that is no keyword, so a flag write no longer writes it back. With serde a keyword is checked on the way in: an index persisted with a token now refused fails to load and has to be rebuilt.

Left open: on Windows the backslash separates segments and is not looked at, as in io-maildir. A mailbox named after a side file (`x.lock`, `x.msf`) still resolves: the listing hides it, and `x.lock` is the dotlock of `x`, which a writer breaks once it looks stale. `X-Keywords` is written on one line whatever its length, and a reader keeps 64 KiB of flag fields per message.

Breaking: `MboxStore::resolve` returns an `Option`, `MboxFlag::Keyword` holds an `MboxKeyword`, `MboxYield`, `MboxReply`, `MboxFileKind` and `MboxClientError` each gained a variant, and a driver has to answer `WantsDirCanonical`.
