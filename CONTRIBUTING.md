# Contributing guide

Thank you for investing your time in contributing to io-mbox.

Whether you are a human or an AI agent, read these in order before touching the code:

1. the [Pimalaya README](https://github.com/pimalaya) for what the project is and how its repositories stack;
2. the [Pimalaya CONTRIBUTING](https://github.com/pimalaya/.github/blob/master/CONTRIBUTING.md) guide, which chains to the shared architecture and guidelines;
3. the inline header documentation, starting with src/lib.rs: it is the architecture document of this crate;
4. the cairn/ folder for the specification and the development history.

Everything below documents only what differs from the Pimalaya standards.

## Feature matrix

The I/O-free coroutines are the featureless no_std core; every cargo feature only gates additional code and dependencies on top. Check the core stays no_std and each layer still builds:

- no default features: the pure no_std core, no std, no filesystem, no parser.
- client: the std-blocking driver over the filesystem, with fcntl locks through libc on Unix.
- parser: mail-parser support exposing the parsed-entry helper.
- serde: serde support for the index and its entries.
- all features: the default set, plus anything gated behind docsrs.

## Corpus

A change to the scanner or to a write path must keep the corpus suite green, public archives included:

```sh
tests/corpus/fetch.sh
nix develop --command cargo test --release --test corpus -- --ignored
```

Private mailboxes can join the run through `IO_MBOX_CORPUS_DIR`, a colon-separated list of directories holding `.mbox` files. They are copied before use and never written.

The interoperability test needs formail and GNU mailutils:

```sh
nix shell nixpkgs#procmail nixpkgs#mailutils -c nix develop --command cargo test --test interop -- --ignored
```

The scanner fuzz target needs a nightly toolchain and cargo-fuzz; seeding it with the fixtures speeds it up:

```sh
mkdir -p fuzz/corpus/scan && cp tests/fixtures/*.mbox fuzz/corpus/scan/
cargo +nightly fuzz run scan fuzz/corpus/scan -- -max_total_time=300
```
