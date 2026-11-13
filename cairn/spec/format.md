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

The crate SHALL read mboxo, mboxrd, mboxcl and mboxcl2, and write any of them, mboxrd by default. mboxo and mboxcl quote a `From ` line with one `>`, mboxrd quotes every `>*From ` line, mboxcl2 quotes nothing. Reading undoes the quoting of the configured variant. A `Content-Length` field SHALL be trusted only when the reader is told the file is an mboxcl variant, and only when the body range it declares ends on a message boundary: a blank line then a `From_` line, or the end of the file, as mutt checks it. A trusted length keeps any `From_` line inside its range from opening a message; a length off the boundary SHALL be ignored, the message split where it would be without it. A `Content-Length` declared inside four ranges not yet checked, or whose range would end past the largest file offset, SHALL be ignored too.

#### Scenario: A Content-Length running past the next message

- GIVEN an mboxcl2 file whose second message declares a `Content-Length` longer than its body, landing past the end of the file or inside a later message
- WHEN the file is scanned
- THEN every `From_` line after a blank line opens its own message, as in an mboxrd scan

### Requirement: Flags live in c-client fields

Flags SHALL be read from `Status` (`R` seen, `O` old), `X-Status` (`A` answered, `F` flagged, `T` draft, `D` deleted) and `X-Keywords` (keywords separated by spaces or commas). A message carrying none of them SHALL have its flags read from `X-Mozilla-Status` and `X-Mozilla-Keys`. Flags SHALL be written as `Status`, `X-Status` and `X-Keywords`, each omitted when empty. A flag write on a message carrying an `X-Mozilla-Status` of four hex digits SHALL also set its read (`0x0001`), replied (`0x0002`), marked (`0x0004`) and expunged (`0x0008`) bits from the flags, in place, its other bits kept, so Thunderbird, which reads that field first, agrees. A flag write on a message carrying `X-Mozilla-Keys` SHALL write that field again after the flag fields, from the keywords, so Thunderbird keeps the labels Himalaya sets or drops. No Thunderbird field is ever added to a message lacking it.

### Requirement: The pseudo message is metadata

A message carrying an `X-IMAP` field is the c-client folder metadata message. It SHALL be marked on its entry so clients hide it, and SHALL be preserved by rewrites like any other message.
