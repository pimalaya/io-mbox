---
cairn: spec
capability: client
status: current
---

# Std client

How the std driver answers the coroutines.

### Requirement: fcntl locks conflict with MTAs

On Linux the fcntl lock SHALL be an open file description lock (`F_OFD_SETLK`), which conflicts with the classic POSIX locks MTAs take, and a classic `F_SETLK` on other Unix systems. On Windows it SHALL be a no-op.

### Requirement: A failed run cleans up but keeps recoverable data

When a run fails, the client SHALL remove the dotlocks it created and close the files it opened. It SHALL remove a temporary file it created unless the mbox was written since: the file then holds the only intact copy of the mbox tail, and SHALL be kept and named in the error, with the mbox and the offset its first byte belongs at. An append whose write fails SHALL truncate the file back to its previous size.

### Requirement: The temporary file of a rewrite is private and beside the mbox

The temporary file of a rewrite SHALL be created in the configured directory when there is one, else in the directory of the mbox, hidden, so it shares its filesystem, and else in the system temporary directory when the directory of the mbox cannot be written to. On Unix it SHALL be readable and writable by its owner alone.

### Requirement: A dotlock is only touched while it is this run's

A dotlock SHALL hold the pid of the process. The client SHALL refresh, release or clean up a dotlock only while the file still is the one this run created, by its inode and the pid it holds. A dotlock another writer replaced SHALL be left in place, and releasing a dotlock already gone SHALL succeed.

### Requirement: A denied dotlock names the option

Failing to create a dotlock for lack of permission, as in a spool directory the user cannot write to, SHALL fail with an error telling to skip dotlocking.

### Requirement: A busy lock is told apart

The client error SHALL tell a lock another process held past the timeout, whichever write wanted it, from any other failure, so a caller can offer a retry.

### Requirement: Listing leaves out files that are no mbox

The client's listing SHALL leave out a file below the root that does not open on a `From_` line, leading blank lines aside, as mutt refuses such a file. An empty file, a file it cannot read and the spool SHALL stay listed, but a spool that is a directory SHALL be left out.
