use actix_web::get;
use actix_web::web;
use actix_web::App;
use actix_web::HttpResponse;
use actix_web::HttpServer;
use actix_web::Responder;
use anyhow::Context;
use clap::Parser;
use indoc::indoc;
use rusqlite::OpenFlags;
use rusqlite::ToSql;
use serde::Deserialize;
use serde::Serialize;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex;
use tokio::runtime::Runtime;
use url::Url;

const PORT: u16 = 31200;

#[derive(Debug, Parser)]
pub struct ReportCommand {
    /// Path to sqlite file previously created with `index` command
    db: PathBuf,
}

impl ReportCommand {
    pub fn run(self) -> anyhow::Result<()> {
        let rt = Runtime::new().unwrap();
        rt.block_on(self.run_impl())
    }

    pub async fn run_impl(self) -> anyhow::Result<()> {
        let conn =
            rusqlite::Connection::open_with_flags(&self.db, OpenFlags::SQLITE_OPEN_READ_ONLY)
                .with_context(|| format!("failed to open db {}", self.db.display()))?;

        let db = self
            .db
            .file_name()
            .map(|n| Path::new(n).to_owned())
            .unwrap_or(self.db);

        let total_size: u64 = conn.query_row("SELECT SUM(size) FROM mail", (), |row| row.get(0))?;

        // Load templates.
        let index_html = include_str!("index.html");
        let mut tera = tera::Tera::default();
        tera.add_raw_template("index", index_html).unwrap();

        // Shared worker state.
        let state = Arc::new(AppState {
            db,
            total_size,
            tera,
            conn: Mutex::new(conn),
        });

        // Bind to a local address.
        let server = HttpServer::new(move || {
            App::new()
                .app_data(web::Data::new(state.clone()))
                .service(index)
        })
        .bind(("127.0.0.1", PORT))?;

        println!("Started server on http://localhost:{}/", PORT);

        Ok(server.run().await?)
    }
}

struct AppState {
    db: PathBuf,
    total_size: u64,
    tera: tera::Tera,
    conn: Mutex<rusqlite::Connection>,
}

#[derive(Serialize)]
struct ActiveFilter {
    key: String,
    remove_url: String,
}

#[derive(Serialize)]
struct ByString {
    key: String,
    size: String,
    filter_url: String,
}

#[derive(Serialize)]
struct Mail {
    from: String,
    size: String,
    subject: String,
    raw_date: Option<String>,
}

#[derive(Clone, Deserialize)]
struct Filters {
    label: Option<String>,
    year: Option<String>,
    domain: Option<String>,
    address: Option<String>,
}

impl Filters {
    fn to_url(&self) -> String {
        let mut base = Url::parse("fake:/").unwrap();
        let mut qp = base.query_pairs_mut();
        self.label.as_ref().map(|s| qp.append_pair("label", s));
        self.year.as_ref().map(|s| qp.append_pair("year", s));
        self.domain.as_ref().map(|s| qp.append_pair("domain", s));
        self.address.as_ref().map(|s| qp.append_pair("address", s));
        drop(qp);
        return base.as_str().strip_prefix("fake:").unwrap().to_owned();
    }

    fn has_any(&self) -> bool {
        self.label.is_some()
            || self.year.is_some()
            || self.domain.is_some()
            || self.address.is_some()
    }

    fn clause(&self) -> String {
        let mut words = vec![];
        if self.label.is_some() {
            words.extend_from_slice(&[
                "JOIN",
                "labels",
                "ON",
                "labels.mail_rowid",
                "=",
                "mail._rowid_",
            ]);
        }
        let mut add_clause = |column, value: Option<&String>| {
            if value.is_some() {
                words.push(if words.is_empty() { "WHERE" } else { "AND" });
                words.extend_from_slice(&[column, "=", "?"]);
            }
        };
        add_clause("labels.label", self.label.as_ref());
        add_clause(
            "strftime('%Y', datetime(date, 'unixepoch'))",
            self.year.as_ref(),
        );
        add_clause(
            "substr(from_address, instr(from_address, '@') + 1)",
            self.domain.as_ref(),
        );
        add_clause("from_address", self.address.as_ref());
        words.join(" ")
    }

    fn params<'a>(&'a self) -> Vec<&'a dyn ToSql> {
        let mut params: Vec<&'a dyn ToSql> = vec![];
        let mut add_param = |value: Option<&'a String>| {
            if let Some(value) = value {
                params.push(value as &dyn ToSql);
            }
        };
        add_param(self.label.as_ref());
        add_param(self.year.as_ref());
        add_param(self.domain.as_ref());
        add_param(self.address.as_ref());
        params
    }
}

#[get("/")]
async fn index(data: web::Data<Arc<AppState>>, query: web::Query<Filters>) -> impl Responder {
    let filters = query.into_inner();

    let mut active_filters = Vec::new();
    if let Some(key) = filters.label.as_ref() {
        active_filters.push(ActiveFilter {
            key: key.to_owned(),
            remove_url: Filters {
                label: None,
                ..filters.clone()
            }
            .to_url(),
        });
    }
    if let Some(key) = filters.year.as_ref() {
        active_filters.push(ActiveFilter {
            key: key.to_owned(),
            remove_url: Filters {
                year: None,
                ..filters.clone()
            }
            .to_url(),
        });
    }
    if let Some(key) = filters.domain.as_ref() {
        active_filters.push(ActiveFilter {
            key: key.to_owned(),
            remove_url: Filters {
                domain: None,
                ..filters.clone()
            }
            .to_url(),
        });
    }
    if let Some(key) = filters.address.as_ref() {
        active_filters.push(ActiveFilter {
            key: key.to_owned(),
            remove_url: Filters {
                address: None,
                ..filters.clone()
            }
            .to_url(),
        });
    }

    let options = humansize::FormatSizeOptions::from(humansize::DECIMAL).decimal_places(2);

    let conn = data.conn.lock().unwrap();

    let filtered_size = if filters.has_any() {
        conn.query_row(
            &format!("SELECT SUM(size) FROM mail {}", filters.clause()),
            filters.params().as_slice(),
            |row| row.get(0),
        )
        .unwrap()
    } else {
        data.total_size
    };

    let mut by_label = Vec::new();
    if filters.label.is_none() {
        let mut stmt = conn
            .prepare(&format!(
                indoc! {r#"
                    SELECT labels.label, sum(size) as total_size
                    FROM labels
                    JOIN mail ON labels.mail_rowid = mail._rowid_
                    {}
                    GROUP BY label
                    ORDER BY total_size DESC
                "#},
                filters.clause()
            ))
            .expect("must be valid syntax");

        let mut rows = stmt
            .query(filters.params().as_slice())
            .expect("query failed");
        while let Some(row) = rows.next().expect("next failed") {
            let label: String = row.get(0).expect("expected column 0");
            by_label.push(ByString {
                key: label.clone(),
                size: humansize::format_size(
                    row.get::<usize, u64>(1).expect("expected column 1"),
                    options,
                ),
                filter_url: Filters {
                    label: Some(label),
                    ..filters.clone()
                }
                .to_url(),
            });
        }
    }

    let mut by_year = Vec::new();
    if filters.year.is_none() {
        let mut stmt = conn
            .prepare(&format!(
                indoc! {r#"
                    SELECT strftime("%Y", datetime(date, 'unixepoch')) as year, sum(size) as total_size
                    FROM mail
                    {}
                    GROUP BY year
                    ORDER BY total_size DESC
                "#},
                filters.clause()
            ))
            .expect("must be valid syntax");

        let mut rows = stmt
            .query(filters.params().as_slice())
            .expect("query failed");
        while let Some(row) = rows.next().expect("next failed") {
            let year: String = row.get(0).expect("expected column 0");
            by_year.push(ByString {
                key: year.clone(),
                size: humansize::format_size(
                    row.get::<usize, u64>(1).expect("expected column 1"),
                    options,
                ),
                filter_url: Filters {
                    year: Some(year),
                    ..filters.clone()
                }
                .to_url(),
            });
        }
    }

    let mut by_domain = Vec::new();
    if filters.domain.is_none() {
        let mut stmt = conn
            .prepare(&format!(
                indoc! {r#"
                    SELECT substr(from_address, instr(from_address, '@') + 1) as domain, sum(size) as total_size
                    FROM mail
                    {}
                    GROUP BY domain
                    ORDER BY total_size DESC
                    LIMIT 30
                "#},
                filters.clause()
            ))
            .expect("must be valid syntax");

        let mut rows = stmt
            .query(filters.params().as_slice())
            .expect("query failed");
        while let Some(row) = rows.next().expect("next failed") {
            let domain: String = row.get(0).expect("expected column 0");
            by_domain.push(ByString {
                key: domain.clone(),
                size: humansize::format_size(
                    row.get::<usize, u64>(1).expect("expected column 1"),
                    options,
                ),
                filter_url: Filters {
                    domain: Some(domain),
                    ..filters.clone()
                }
                .to_url(),
            });
        }
    }

    let mut by_address = Vec::new();
    if filters.address.is_none() {
        let mut stmt = conn
            .prepare(&format!(
                indoc! {r#"
                    SELECT from_address, sum(size) as total_size
                    FROM mail
                    {}
                    GROUP BY from_address
                    ORDER BY total_size DESC
                    LIMIT 30
                "#},
                filters.clause()
            ))
            .expect("must be valid syntax");

        let mut rows = stmt
            .query(filters.params().as_slice())
            .expect("query failed");
        while let Some(row) = rows.next().expect("next failed") {
            let address: String = row.get(0).expect("expected column 0");
            by_address.push(ByString {
                key: address.clone(),
                size: humansize::format_size(
                    row.get::<usize, u64>(1).expect("expected column 1"),
                    options,
                ),
                filter_url: Filters {
                    address: Some(address),
                    ..filters.clone()
                }
                .to_url(),
            });
        }
    }

    let mut stmt = conn
        .prepare(&format!(
            indoc! {r#"
                SELECT from_address, size, subject, raw_date
                FROM mail
                {}
                ORDER BY size DESC
                LIMIT 30
            "#},
            filters.clause()
        ))
        .expect("must be valid syntax");

    let mut top_mail = Vec::new();
    let mut rows = stmt
        .query(filters.params().as_slice())
        .expect("query failed");
    while let Some(row) = rows.next().expect("next failed") {
        top_mail.push(Mail {
            from: row.get(0).expect("expected column 0"),
            size: humansize::format_size(
                row.get::<usize, u64>(1).expect("expected column 1"),
                options,
            ),
            subject: row.get(2).expect("expected column 2"),
            raw_date: row.get(3).expect("expected column 3"),
        });
    }

    let mut context = tera::Context::new();
    context.insert("db", &data.db.display().to_string());
    context.insert(
        "total_size",
        &humansize::format_size(data.total_size, options),
    );
    context.insert(
        "filtered_size",
        &humansize::format_size(filtered_size, options),
    );
    context.insert("active_filters", &active_filters);
    context.insert("by_label", &by_label);
    context.insert("by_year", &by_year);
    context.insert("by_domain", &by_domain);
    context.insert("by_address", &by_address);
    context.insert("top_mail", &top_mail);
    HttpResponse::Ok().body(data.tera.render("index", &context).unwrap())
}
