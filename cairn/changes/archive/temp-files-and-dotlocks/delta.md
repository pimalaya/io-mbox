---
cairn: delta
change: temp-files-and-dotlocks
---

## ADDED Requirements

### Requirement: The temporary file of a rewrite is private and beside the mbox

The temporary file of a rewrite SHALL be created in the configured directory when there is one, else in the directory of the mbox, hidden, so it shares its filesystem, and else in the system temporary directory when the directory of the mbox cannot be written to. On Unix it SHALL be readable and writable by its owner alone.

### Requirement: A dotlock is only touched while it is this run's

A dotlock SHALL hold the pid of the process. The client SHALL refresh, release or clean up a dotlock only while the file still is the one this run created, by its inode and the pid it holds. A dotlock another writer replaced SHALL be left in place, and releasing a dotlock already gone SHALL succeed.

## MODIFIED Requirements

### Requirement: A failed run cleans up but keeps recoverable data

When a run fails, the client SHALL remove the dotlocks it created and close the files it opened. It SHALL remove a temporary file it created unless the mbox was written since: the file then holds the only intact copy of the mbox tail, and SHALL be kept and named in the error, with the mbox and the offset its first byte belongs at. An append whose write fails SHALL truncate the file back to its previous size.

### Requirement: Every write is locked

A rewrite SHALL refresh its dotlock every 256 reads, 16 MiB at the default read size, so a rewrite longer than the stale age keeps it.

### Requirement: A name stays below the root

A name with a segment ending in `.lock` or `.msf` SHALL NOT resolve either: it names the dotlock or the summary of the mbox beside it, which a write to that mbox removes. The listing SHALL NOT walk a directory named so.

### Requirement: Rewrites stay in place

The entries to edit SHALL be found in one pass over the index, whatever the number of edits.

## REMOVED Requirements
