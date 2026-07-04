//! Integration tests against a small "bookstore" schema. They require a
//! reachable PostgreSQL via `$DATABASE_URL`; they are skipped otherwise.
//!
//! The schema (see `tests/data/bookstore/bookstore-db.sql`) exercises the same
//! code paths the tool cares about: foreign-key dependency ordering
//! (author/publisher/customer → book → book_order → order_line), a deferrable
//! UNIQUE constraint (`book.isbn`), a GENERATED column (`book.price_with_tax`),
//! a pre-commit function (`recompute_sales_summary`), a config table touched via
//! `--pre-commit-sql`, and a trigger enforcing "at most one featured book".

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use csv_pg_injector::dburl::parse_database_url;
use postgres::{Client, NoTls};

const BIN: &str = env!("CARGO_BIN_EXE_csv-pg-injector");

/// The tests share a single database, so serialize them (cargo runs tests in
/// parallel by default).
static DB_LOCK: Mutex<()> = Mutex::new(());

fn lock_db() -> std::sync::MutexGuard<'static, ()> {
    DB_LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

fn data_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data")
}

fn bookstore_dir() -> PathBuf {
    data_dir().join("bookstore")
}

fn schema_sql() -> PathBuf {
    bookstore_dir().join("bookstore-db.sql")
}

fn database_url() -> Option<String> {
    std::env::var("DATABASE_URL").ok()
}

fn connect() -> Client {
    let url = database_url().expect("DATABASE_URL set");
    let cfg = parse_database_url(&url);
    Client::configure()
        .host(cfg.host.as_deref().unwrap_or("localhost"))
        .port(cfg.port.unwrap_or(5432))
        .user(cfg.user.as_deref().unwrap_or("dbuser"))
        .dbname(cfg.dbname.as_deref().unwrap_or("dbname"))
        .password(cfg.password.as_deref().unwrap_or(""))
        .connect(NoTls)
        .expect("connect to database")
}

/// Apply the (idempotent) schema before a test.
fn create_database_if_needed() {
    let sql = std::fs::read_to_string(schema_sql()).expect("read schema sql");
    connect()
        .batch_execute(&sql)
        .expect("apply bookstore schema");
}

fn unique_prom() -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    std::env::temp_dir().join(format!("csv_pg_injector_{nanos}.prom"))
}

fn run_injector(extra_args: &[&str], target: &Path, prom: &Path) -> std::process::ExitStatus {
    Command::new(BIN)
        .args(extra_args)
        .arg("--prom-file")
        .arg(prom)
        .arg(target)
        .status()
        .expect("spawn csv-pg-injector")
}

#[test]
fn test_injection() {
    let Some(_url) = database_url() else {
        eprintln!("skipping test_injection: DATABASE_URL not set");
        return;
    };
    let _guard = lock_db();
    let bookstore = bookstore_dir();

    let cases: Vec<Vec<&str>> = vec![
        // Plain load of the whole directory.
        vec![],
        // Load with pre-commit function calls (three accepted spellings), a
        // pre-commit UPDATE, a non-default isolation level and work_mem.
        vec![
            "--pre-commit-sql",
            "function public.recompute_sales_summary",
            "--pre-commit-sql",
            "function public.recompute_sales_summary()",
            "--pre-commit-sql",
            "function public.recompute_sales_summary();",
            "--pre-commit-sql",
            "UPDATE public.config SET val='pouet' where config_id = 666",
            "--isolation-level",
            "repeatable-read",
            "--work-mem",
            "512MB",
        ],
    ];

    for extra in cases {
        create_database_if_needed();
        let prom = unique_prom();
        let status = run_injector(&extra, &bookstore, &prom);
        assert!(status.success(), "injection failed for args {extra:?}");
        assert!(prom.exists(), "prom file not written");
        let _ = std::fs::remove_file(&prom);
    }

    // The whole dataset should have landed with the expected row counts.
    let mut client = connect();
    for (table, expected) in [
        ("author", 5i64),
        ("publisher", 3),
        ("customer", 4),
        ("book", 8),
        ("book_order", 5),
        ("order_line", 12),
    ] {
        let count: i64 = client
            .query_one(&format!("SELECT count(*) FROM {table}"), &[])
            .unwrap()
            .get(0);
        assert_eq!(count, expected, "unexpected row count for {table}");
    }
    // The pre-commit function rebuilt the derived table.
    let summary: i64 = client
        .query_one("SELECT count(*) FROM sales_summary", &[])
        .unwrap()
        .get(0);
    assert!(
        summary > 0,
        "recompute_sales_summary did not populate sales_summary"
    );
    // The pre-commit UPDATE took effect.
    let val: String = client
        .query_one("SELECT val FROM config WHERE config_id = 666", &[])
        .unwrap()
        .get(0);
    assert_eq!(val, "pouet");
}

#[test]
fn test_injection_failing() {
    let Some(_url) = database_url() else {
        eprintln!("skipping test_injection_failing: DATABASE_URL not set");
        return;
    };
    let _guard = lock_db();
    let bookstore = bookstore_dir();

    let cases: Vec<(Vec<&str>, PathBuf)> = vec![
        // Calling a function that does not exist.
        (
            vec![
                "--pre-commit-sql",
                "function public.recompute_sales_summary_that_does_not_exist()",
            ],
            bookstore.clone(),
        ),
        // Updating a table that does not exist.
        (
            vec![
                "--pre-commit-sql",
                "UPDATE non_existing_table set val='x' where id = 666;",
            ],
            bookstore.clone(),
        ),
        // Two featured books -> the enforce_single_featured_book trigger raises.
        (vec![], data_dir().join("bookstore_fail")),
    ];

    for (extra, target) in cases {
        create_database_if_needed();
        let prom = unique_prom();
        let status = run_injector(&extra, &target, &prom);
        assert!(!status.success(), "expected failure for args {extra:?}");
        let _ = std::fs::remove_file(&prom);
    }
}

#[test]
fn test_run_twice() {
    let Some(_url) = database_url() else {
        eprintln!("skipping test_run_twice: DATABASE_URL not set");
        return;
    };
    let _guard = lock_db();
    let different_order = data_dir().join("bookstore_different_order");

    let cases: Vec<(PathBuf, PathBuf)> = vec![
        // Same full directory twice (idempotent upsert + delete).
        (bookstore_dir(), bookstore_dir()),
        // Single table with the rows in a different order between runs.
        (
            different_order.join("first_run/author.csv"),
            different_order.join("second_run/author.csv"),
        ),
    ];

    for (run1, run2) in cases {
        create_database_if_needed();
        let prom = unique_prom();
        assert!(
            run_injector(&[], &run1, &prom).success(),
            "first run failed"
        );
        assert!(
            run_injector(&[], &run2, &prom).success(),
            "second run failed"
        );
        let _ = std::fs::remove_file(&prom);
    }
}
