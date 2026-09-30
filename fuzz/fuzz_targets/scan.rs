//! Feeds arbitrary bytes to the scanner in two different chunkings and
//! checks they agree, and that every entry lies inside the input and
//! scans back to itself when its range is scanned alone.
//!
//! See CONTRIBUTING.md for how to run it.

#![no_main]

use io_mbox::scan::{MboxScanner, MboxScannerOptions};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|input: &[u8]| {
    let Some((&seed, bytes)) = input.split_first() else {
        return;
    };
    let opts = MboxScannerOptions {
        content_length: seed & 1 == 1,
        header: seed & 2 == 2,
    };
    let chunk = (seed as usize >> 2) + 1;

    let scan = |chunk: usize| {
        let mut scanner = MboxScanner::new(0, opts.clone());
        let mut entries = Vec::new();
        for piece in bytes.chunks(chunk) {
            entries.extend(scanner.feed(piece));
        }
        entries.extend(scanner.finish());
        entries
    };

    let whole = scan(bytes.len().max(1));
    assert_eq!(scan(chunk), whole);

    let mut previous_end = 0;
    for entry in &whole {
        assert!(entry.offset >= previous_end && entry.offset < entry.message_offset);
        assert!(entry.end() <= bytes.len() as u64 && entry.header_len <= entry.len);
        previous_end = entry.end();

        let range = &bytes[entry.offset as usize..entry.end() as usize];
        let mut scanner = MboxScanner::new(entry.offset, opts.clone());
        let mut alone = scanner.feed(range);
        if range.ends_with(b"\n") {
            alone.extend(scanner.feed(b"\n"));
        }
        alone.extend(scanner.finish());
        assert_eq!(alone.len(), 1);
        assert_eq!(alone[0].id, entry.id.split('-').next().unwrap());
    }
});
