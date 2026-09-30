# Test fixtures

Small mbox files committed with the crate. The corpus suite (tests/corpus.rs) runs its invariants on each of them.

- mboxshell_*.mbox: fixtures of [mboxshell](https://github.com/dcarrero/mboxshell), MIT, copyright Tecnologia y Sistemas Carrero.
- mail_parser_sample.mbox: fixture of [mail-parser](https://github.com/stalwartlabs/mail-parser), MIT OR Apache-2.0, copyright Stalwart Labs.
- synth_*.mbox: written for this crate, covering mboxcl2 bodies holding a `From_` line, CRLF line endings, Thunderbird flags, and the c-client pseudo message.
