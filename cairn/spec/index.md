---
cairn: spec
capability: index
status: current
---

# Index

How the entries of a file are kept and brought up to date.

### Requirement: An index describes one file state

An index SHALL carry the entries, the scanner options they were produced with, the file stat (size, modification time, inode) and a hash of the last 4 KiB of the file, and SHALL be serialisable with the `serde` feature. The crate SHALL NOT store it anywhere itself.

### Requirement: Sync scans only what changed

Syncing SHALL reuse an index whose stat matches the file and whose options match the requested ones. A file that only grew, on the same inode, with the old last 4 KiB unchanged SHALL be scanned from the last entry of the index onwards. Any other change SHALL trigger a full scan. The result SHALL equal a full scan as long as nothing before the last entry of the index was rewritten in place. Such a rewrite followed by growth is not seen: the entries before keep the flags and field places the index held, which is why reads and rewrites check the entries they touch. Each read SHALL ask for the read size or for what is left of the file, whichever is smaller, whatever the file size and the word size of the target.

### Requirement: Reads check the entry

Reading a message SHALL scan its bytes again and fail as stale, asking for a sync, when they no longer hold a message with the entry's content id and length.
