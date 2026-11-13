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

When a run fails, the client SHALL remove the dotlocks it created and close the files it opened. It SHALL keep the temporary files it created and name one in the error. An append whose write fails SHALL truncate the file back to its previous size.

### Requirement: A denied dotlock names the option

Failing to create a dotlock for lack of permission, as in a spool directory the user cannot write to, SHALL fail with an error telling to skip dotlocking.

### Requirement: A busy lock is told apart

The client error SHALL tell a lock another process held past the timeout, whichever write wanted it, from any other failure, so a caller can offer a retry.

### Requirement: Listing leaves out files that are no mbox

The client's listing SHALL leave out a file below the root that does not open on a `From_` line, leading blank lines aside, as mutt refuses such a file. An empty file, a file it cannot read and the spool SHALL stay listed, but a spool that is a directory SHALL be left out.
