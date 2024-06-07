# gmail-mbox-analyzer

If your Google Mail is full and you want to understand why,
gmail-mbox-analyzer may help.

This tool has two functions:
* Converts an mbox file, probably exported from [Google
  Takeout](https://takeout.google.com/), into a SQLite database.
* Provides a gloriously HTML 1.0 UI for drilling down by label, year,
  domain, and sender.

## Installation

* Ensure Rust is installed, probably via [rustup](https://rustup.rs/).
* From a shell: `cargo install gmail-mbox-analyzer`
* You may need to install sqlite3 system libraries. For example, on
  Ubuntu, `sudo apt install libsqlite3-dev`

## Usage

First, retrieve your mbox file from Takeout.

Then, from a command line, convert it to a SQLite database:

```
gmail-mbox-analyzer index "All mail Including Spam and Trash.mbox" mail.sqlite
```

Finally, load the report view:

```
gmail-mbox-analyzer report mail.sqlite
```

It will tell you to load a URL like http://localhost:31200/

## Credits

This software wouldn't exist without the excellent
[mbox-reader](https://docs.rs/mbox-reader/latest/mbox_reader/),
[mail-parser](https://docs.rs/mail-parser/), and
[rusqlite](https://docs.rs/rusqlite/) crates. With them, it only took
a few evenings.

Special thanks to [actix-web](https://docs.rs/actix-web/),
[https://docs.rs/tera/], and [https://docs.rs/humansize/].
