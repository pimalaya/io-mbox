//! Lists the messages of an mbox file with the std client.
//!
//! ```sh
//! cargo run --example std_list_entries -- /var/mail/$USER
//! ```

use std::{env, path::Path};

use io_mbox::client::MboxClient;

fn main() {
    let _ = env_logger::try_init();

    let path = env::args().nth(1).expect("usage: std_list_entries <mbox>");
    let path = Path::new(&path);
    let root = path.parent().unwrap_or(Path::new("."));
    let name = path.file_name().expect("mbox file name").to_string_lossy();

    let client = MboxClient::new(root);
    let index = client.sync_index(name.as_ref(), None).unwrap().index;

    for entry in index.messages() {
        let message = client.get(name.as_ref(), entry.clone()).unwrap();
        let subject = message
            .parsed()
            .and_then(|m| m.subject().map(String::from))
            .unwrap_or_default();
        let flags: Vec<String> = entry.flags.iter().map(|f| f.to_string()).collect();
        println!("{} [{}] {subject}", entry.id, flags.join(","));
    }
}
