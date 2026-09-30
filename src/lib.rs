#![no_std]
#![cfg_attr(docsrs, feature(doc_cfg))]

//! # io-mbox
//!
//! I/O-free mbox coroutines: every filesystem access is a resumable state
//! machine that yields a request (read these bytes at this offset, take
//! this lock, give me the time) instead of performing it. The caller owns
//! the syscalls and resumes the coroutine with the answers, whatever the
//! runtime. The `client` feature ships a std-blocking driver.
//!
//! ## The format
//!
//! An mbox is one file holding every message of a mailbox, each opened
//! by a `From_` line and followed by a blank line. It is a family of
//! de-facto formats rather than a standard: the four variants differ in
//! how a body line starting with `From ` is quoted, and flags live in
//! header fields. The crate reads all of them, writes mboxrd by default,
//! and follows the c-client conventions (UW-IMAP, Dovecot, mutt) for
//! flags, locking and rewrites, so the files it writes stay readable by
//! the tools that share them.
//!
//! ## Layout
//!
//! The pure parts come first. [`from_line`] parses and formats the
//! `From_` line, [`mod@format`] quotes and unquotes per variant, [`flag`]
//! reads and writes the flag fields, and [`scan`] holds
//! [`scan::MboxScanner`], the streaming parser turning chunks of a file
//! into [`entry::MboxEntry`]s with content ids. None of them does I/O or
//! is a coroutine.
//!
//! The coroutines build on them. [`index`] keeps the entries of a file
//! with the file state they describe, and [`index::sync`] brings them up
//! to date, scanning only what changed. [`entry`] reads messages
//! ([`entry::get`]), appends them ([`entry::append`]), rewrites flags and
//! removes messages in place ([`entry::update`]), and copies or moves
//! them between files ([`entry::copy`], [`entry::move`]). [`lock`] takes
//! and releases the dotlock and the fcntl lock around every write, and
//! [`range`] copies byte ranges chunk by chunk for the rewrites. [`mbox`]
//! creates, deletes, lists and renames whole mailboxes, and [`store`]
//! maps mailbox names to files.
//!
//! Shared across them, [`coroutine`] holds the
//! [`coroutine::MboxCoroutine`] trait, its [`coroutine::MboxYield`] and
//! [`coroutine::MboxReply`] vocabulary and the [`mbox_try!`] macro, and
//! [`path`] the filesystem and logical path types. The optional
//! [`client`] module (`client` feature) is the std driver,
//! [`client::MboxClient`].
//!
//! ## Memory
//!
//! Reads are chunked and the scanner keeps at most one line, one header
//! and the volatile fields of the message in progress, so scanning a file
//! of any size runs in bounded memory. Rewrites copy through a temporary
//! file rather than memory. Only a message being read or appended is held
//! whole.
//!
//! ## Naming
//!
//! Public types follow the Domain-Target-Verb scheme
//! ([`entry::get::MboxEntryGet`], [`index::sync::MboxIndexSync`]) with
//! Error, Options and Output companions.
//!
//! ## Features
//!
//! The coroutines are always present and need no feature. `client` adds
//! the std driver, `parser` pulls in mail-parser to expose
//! [`entry::MboxFullEntry::parsed`], and `serde` derives serde for the
//! index and its entries, so a caller can persist them.

#[macro_use]
extern crate alloc;
#[cfg(feature = "client")]
extern crate std;

#[cfg(feature = "client")]
pub mod client;
pub mod coroutine;
pub mod entry;
pub mod flag;
pub mod format;
pub mod from_line;
mod header;
pub mod index;
pub mod lock;
pub mod mbox;
pub mod path;
pub mod range;
pub mod scan;
pub mod store;
