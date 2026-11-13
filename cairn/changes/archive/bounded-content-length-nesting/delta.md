---
cairn: delta
change: bounded-content-length-nesting
---

## ADDED Requirements

### Requirement: Content-Length checks are bounded

While the body range of a trusted `Content-Length` is unchecked, the scanner SHALL keep a second scan of that range, from its first `From_` line on, that ignores the length, and SHALL hand over to it when the range ends off a message boundary. A message of that second scan may open a range of its own. Unchecked ranges SHALL nest four deep at most, a `Content-Length` declared inside four of them being ignored, so a line is scanned five times at most whatever the file nests. A range proved right or wrong SHALL no longer count. When nested ranges are proved wrong at the same line, each of them SHALL be ignored and no message SHALL be reported twice.

#### Scenario: Thousands of nested lengths

- GIVEN a message whose body repeats a `From_` line, a `Content-Length` and a blank line 4,000 times
- WHEN the file is scanned trusting `Content-Length`
- THEN at most four scans run beside the main one, and the scan fits a 256 KiB stack

## MODIFIED Requirements

### Requirement: Scanning is streaming

The bounds hold for each of the five scans a file can nest. Entries completed inside an unchecked `Content-Length` range are held until the range is checked.

### Requirement: Four variants

A `Content-Length` declared inside four ranges not yet checked, or whose range would end past the largest file offset, SHALL be ignored too.

### Requirement: Sync scans only what changed

Each read SHALL ask for the read size or for what is left of the file, whichever is smaller, whatever the file size and the word size of the target.

## REMOVED Requirements
