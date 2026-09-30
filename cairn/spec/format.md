---
cairn: spec
capability: format
status: current
---

# mbox format

What the crate recognises as an mbox, and how it quotes, unquotes and stores flags.

### Requirement: A From_ line is checked, not guessed

A line SHALL open a message only when it starts with `From `, follows a blank line (or opens the file, or ends the bytes before the first message), and carries a full asctime date: day name, month name, day of month, time, and a year, a timezone allowed before or after the year. The sender MAY be empty, quoted with spaces, `-` or `MAILER-DAEMON`, and fields MAY be separated by any whitespace run.

#### Scenario: A body line starting with From

- GIVEN a message whose body holds `From the man page:` after a blank line
- WHEN the file is scanned
- THEN the line stays in the body

### Requirement: Four variants

The crate SHALL read mboxo, mboxrd, mboxcl and mboxcl2, and write any of them, mboxrd by default. mboxo and mboxcl quote a `From ` line with one `>`, mboxrd quotes every `>*From ` line, mboxcl2 quotes nothing. Reading undoes the quoting of the configured variant. A `Content-Length` field SHALL be trusted only when the reader is told the file is an mboxcl variant, and then no `From_` line inside the body range it declares opens a message.

### Requirement: Flags live in c-client fields

Flags SHALL be read from `Status` (`R` seen, `O` old), `X-Status` (`A` answered, `F` flagged, `T` draft, `D` deleted) and `X-Keywords` (keywords separated by spaces or commas). A message carrying none of them SHALL have its flags read from `X-Mozilla-Status` and `X-Mozilla-Keys`. Flags SHALL be written as `Status`, `X-Status` and `X-Keywords` only, each omitted when empty.

### Requirement: The pseudo message is metadata

A message carrying an `X-IMAP` field is the c-client folder metadata message. It SHALL be marked on its entry so clients hide it, and SHALL be preserved by rewrites like any other message.
