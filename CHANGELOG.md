# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- `MboxClientError::lock_busy`, telling a lock another process held past the timeout from any other failure.
- `MboxEntry::mozilla_status`, locating the `X-Mozilla-Status` digits of a message.
- `MboxEntry::mozilla_keys` and `MboxFlags::to_mozilla_keys`, so a flag write writes Thunderbird's `X-Mozilla-Keys` again from the keywords.
- `MboxKeyword`, a keyword checked to read back as written from `X-Keywords`.
- `MboxPath::names_mbox`, telling a name that stands for a file below a store root.
- `MboxYield::WantsDirCanonical`, `MboxReply::DirCanonical` and `MboxFileKind::DirLink`, which the listing walks links with.

### Changed

- A flag write keeps the read, replied, marked and expunged bits of an existing `X-Mozilla-Status` in step, so Thunderbird agrees.
- An append refuses a non-empty file that does not open on a `From_` line (`MboxEntryAppendError::NotMbox`), and `MboxClient::list_mboxes` leaves such files out.
- Deleting or renaming an mbox removes its Thunderbird `.msf` summary.
- A flag write keeps an existing `X-Mozilla-Keys` in step with the keywords, where a set left the labels Thunderbird reads in place.
- `MboxClient::list_mboxes` leaves out a spool that is a directory.
- An append fails before the write when a message would not read back as one entry (`MboxEntryAppendError::Unreadable`).
- A flag write or a removal given an index scans each message to edit again, and fails before the write when the file no longer holds it (`MboxEntryUpdateError::Stale`).
- `MboxStore::resolve` returns `None` for a name that is empty or holds a `.` or `..` segment or a NUL, and the client fails with `MboxClientError::InvalidName`.
- `MboxFlag::Keyword` holds an `MboxKeyword`, so a keyword with a whitespace, a comma or a control character, or longer than 255 bytes, is refused.

### Fixed

- A `Content-Length` whose body range does not end on a message boundary is ignored, as mutt does, instead of merging the following messages into one.
- A huge lock timeout no longer overflows.
- A `Content-Length` declared inside four unchecked `Content-Length` ranges is ignored, so a line is scanned five times at most however deep a message nests them.
- A `Content-Length` too large for a file offset is ignored instead of overflowing.
- An index sync on a 32-bit target no longer stops early when a multiple of 4 GiB is left to scan.
- `MboxFromLine::format` no longer writes a line that does not read back: a sender too long for the line becomes `MAILER-DAEMON`, the year takes four digits and the date stays within the years 0000 to 9999.
- A flag write starts from the flags and flag fields the file holds, where a resumed or reused index could skip an edit or cut the wrong bytes.
- A listing walks each directory once, where a symbolic link back into the tree was followed again and again.

## [0.1.0] - 2026-09-30

### Added

- Initial release: I/O-free mbox coroutines and a std-blocking client. Streaming scan of the four mbox variants with content ids, a resumable scan index, message read, append (stored with LF line endings), flag rewrite, removal, copy and move under dotlock and fcntl locks, and mailbox create, delete, list and rename ([himalaya#697]).

[himalaya#697]: https://github.com/pimalaya/himalaya/issues/697

[unreleased]: https://github.com/pimalaya/io-mbox/compare/v0.1.0..HEAD
[0.1.0]: https://github.com/pimalaya/io-mbox/compare/root..v0.1.0
