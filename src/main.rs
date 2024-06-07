use anyhow::Context;
use clap::Parser;
use indoc::indoc;
use mail_parser::Address;
use mail_parser::DateTime;
use mail_parser::Message;
use rusqlite::Connection;
use rusqlite_migration::Migrations;
use rusqlite_migration::M;
use std::path::PathBuf;

mod report;

#[derive(Parser, Debug)]
struct Args {
    #[command(subcommand)]
    command: Command,
}

#[derive(Parser, Debug)]
enum Command {
    Index(IndexCommand),
    Report(report::ReportCommand),
}

/// Convert mbox file into sqlite
#[derive(Parser, Debug)]
struct IndexCommand {
    /// Path to mbox file
    mbox: PathBuf,
    /// Path where sqlite file is written
    db: PathBuf,
}

const CREATE_MAIL_TABLE: &str = indoc! {"
CREATE TABLE mail (
    size INTEGER,
    from_address TEXT,
    date INTEGER,
    raw_date TEXT,
    subject TEXT
);

CREATE TABLE labels (
    mail_rowid INTEGER,
    label TEXT,
    PRIMARY KEY (mail_rowid, label)
) WITHOUT ROWID;
"};

fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    match args.command {
        Command::Index(c) => c.run(),
        Command::Report(c) => c.run(),
    }
}

impl IndexCommand {
    fn run(self) -> anyhow::Result<()> {
        let default_date = DateTime::from_timestamp(0);

        let parser = mail_parser::MessageParser::new();

        let mf = mbox_reader::MboxFile::from_file(&self.mbox)
            .with_context(|| format!("failed to open mbox {}", self.mbox.display()))?;

        let mut conn = Connection::open(&self.db)
            .with_context(|| format!("failed to open db {}", self.db.display()))?;
        // speedup: MEMORY ?
        conn.pragma_update(None, "journal_mode", "WAL")
            .context("failed to set journal_mode=WAL")?;
        conn.pragma_update(None, "synchronous", "OFF")
            .context("failed to set synchronous=OFF")?;

        let migrations = Migrations::new(vec![M::up(CREATE_MAIL_TABLE)]);
        migrations
            .to_latest(&mut conn)
            .context("failed to migrate schema")?;

        let mut insert_mail = conn.prepare(indoc! {"
            INSERT INTO mail (size, from_address, date, raw_date, subject)
            VALUES (?, ?, ?, ?, ?)
        "})?;

        let mut insert_label = conn.prepare(indoc! {"
            INSERT INTO labels (mail_rowid, label)
            VALUES (?, ?)
        "})?;

        // On an explicit reindex, delete any existing rows.
        conn.execute("DELETE FROM mail", ())?;
        conn.execute("DELETE FROM labels", ())?;

        // Speedups:
        // - transaction(s)
        // - prepared statements
        // - create indices at the end

        conn.execute("BEGIN", ())?;

        for mail in mf.iter() {
            let Some(raw_message) = mail.message() else {
                println!("No message: {:#?}", mail.start().as_str());
                continue;
            };
            let Some(message) = parser.parse_headers(raw_message) else {
                println!("Unable to parse message");
                continue;
            };

            // TODO: Should we factor in the mbox `from` line?
            // message_size + mail.start().as_str().len()

            let message_size = raw_message.len();
            let from_address = find_from_address(&message).unwrap_or("(unknown sender)");
            let date = message.date().unwrap_or(&default_date);
            let date_raw = message.header_raw("Date");
            let subject = message.subject().unwrap_or("(no subject)");

            let labels = if let Some(gmail_labels) = message.header_raw("X-Gmail-Labels") {
                gmail_labels
                    .split(',')
                    .map(|lbl| lbl.trim().replace(['\n', '\r'], ""))
                    .collect()
            } else {
                vec!["Unlabeled".to_owned()]
            };

            insert_mail.execute((
                message_size,
                from_address,
                date.to_timestamp(),
                date_raw,
                subject,
            ))?;

            let mail_rowid = conn.last_insert_rowid();

            for label in labels {
                insert_label.execute((mail_rowid, label))?;
            }
        }

        conn.execute("COMMIT", ())?;

        Ok(())
    }
}

fn find_from_address<'a>(message: &'a Message<'a>) -> Option<&'a str> {
    let address = message.from().or_else(|| message.sender())?;
    let addr = match address {
        Address::List(addrs) => addrs.first()?,
        Address::Group(groups) => groups.first()?.addresses.first()?,
    };
    addr.address.as_deref()
}
