# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.0] - 2026-09-30

### Added

- Initial release: I/O-free mbox coroutines and a std-blocking client. Streaming scan of the four mbox variants with content ids, a resumable scan index, message read, append (stored with LF line endings), flag rewrite, removal, copy and move under dotlock and fcntl locks, and mailbox create, delete, list and rename ([himalaya#697]).

[himalaya#697]: https://github.com/pimalaya/himalaya/issues/697

[unreleased]: https://github.com/pimalaya/io-mbox/compare/v0.1.0..HEAD
[0.1.0]: https://github.com/pimalaya/io-mbox/compare/root..v0.1.0
