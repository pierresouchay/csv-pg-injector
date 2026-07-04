//! Program orchestration: connect, compute dependencies, inject, delete,

use std::collections::HashSet;
use std::env;
use std::path::Path;
use std::time::Instant;

use anyhow::{Context as _, Result};
use postgres::{Client, NoTls};

use crate::cli::{Cli, IsolationLevel, DEFAULT_TIMING_THRESHOLD};
use crate::dburl::parse_database_url;
use crate::deps::{compute_dependencies, expand_csv_files, table_name_from_path};
use crate::inject::{
    delete_rows, iter_constraints_to_defer, process_sql_call, set_constraint_deferred,
    set_work_mem, update_table_from_csv, Constraint,
};
use crate::metrics::Metrics;
use crate::{debug, debug_enabled, Ctx};

const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Resolved database connection parameters.
struct DbConfig {
    host: String,
    port: u16,
    user: String,
    password: Option<String>,
    dbname: String,
}

fn resolve_db_config(cli: &Cli) -> DbConfig {
    // Precedence: CLI flag > DATABASE_URL > hardcoded default (/ $DB_PASSWORD).
    let url = std::env::var("DATABASE_URL")
        .ok()
        .map(|u| {
            debug!(
                "Parsing DB configuration from DATABASE_URL {}",
                regex::Regex::new(":[^@]*@").unwrap().replace(&u, ":***@")
            );
            parse_database_url(&u)
        })
        .unwrap_or_default();

    DbConfig {
        host: cli
            .db_host
            .clone()
            .or(url.host)
            .unwrap_or_else(|| env::var("PGHOST").unwrap_or("localhost".into()).to_string()),
        port: cli.db_port.or(url.port).unwrap_or(5432),
        user: cli
            .db_user
            .clone()
            .or(url.user)
            .or_else(|| std::env::var("PGUSER").ok())
            .expect("PGUSER not specified"),
        password: cli
            .db_password
            .clone()
            .or(url.password)
            .or_else(|| std::env::var("PGPASSWORD").ok()),
        dbname: cli
            .db_name
            .clone()
            .or(url.dbname)
            .or_else(|| std::env::var("PGDATABASE").ok())
            .expect("PGDATABASE not specified"),
    }
}

fn resolve_work_mem(cli: &Cli) -> String {
    cli.work_mem
        .clone()
        .or_else(|| std::env::var("PG_WORK_MEM").ok())
        .unwrap_or_default()
}

fn pg_isolation(level: IsolationLevel) -> Option<postgres::IsolationLevel> {
    match level {
        IsolationLevel::Default => None,
        IsolationLevel::Serializable => Some(postgres::IsolationLevel::Serializable),
        IsolationLevel::RepeatableRead => Some(postgres::IsolationLevel::RepeatableRead),
        IsolationLevel::ReadCommitted => Some(postgres::IsolationLevel::ReadCommitted),
        IsolationLevel::ReadUncommitted => Some(postgres::IsolationLevel::ReadUncommitted),
    }
}

/// Tracks and reports the top-level ("[TOTAL]") timings.
struct MainTimer<'a> {
    start: Instant,
    ctx: &'a Ctx<'a>,
}

impl<'a> MainTimer<'a> {
    fn new(ctx: &'a Ctx<'a>) -> Self {
        MainTimer {
            start: Instant::now(),
            ctx,
        }
    }
    fn measure(&mut self, step: &str, msg: &str) {
        let dur = self.start.elapsed().as_secs_f64();
        self.ctx.format_measure(dur, msg, "", "[TOTAL]\t");
        self.ctx
            .metrics
            .process_duration
            .with_label_values(&[step])
            .set(dur);
        self.start = Instant::now();
    }
}

fn exception_label(err: &anyhow::Error) -> String {
    for cause in err.chain() {
        if let Some(db) = cause
            .downcast_ref::<postgres::Error>()
            .and_then(|e| e.as_db_error())
        {
            return db.code().code().to_string();
        }
    }
    "Error".to_string()
}

/// Entry point invoked from `main`. Parses arguments, runs the injection and
/// always writes the metrics (even on failure)
pub fn run() -> Result<()> {
    use clap::Parser;
    let cli = Cli::parse();

    let mut timing_threshold = cli.timing_threshold;
    if timing_threshold == DEFAULT_TIMING_THRESHOLD && debug_enabled() {
        // With DEBUG set and threshold untouched, display everything.
        timing_threshold = 0.0;
    }

    let db = resolve_db_config(&cli);
    let work_mem_to_set = resolve_work_mem(&cli);

    let expanded = expand_csv_files(&cli.csv_files);
    let pad = expanded
        .iter()
        .map(|p| table_name_from_path(p).len())
        .max()
        .unwrap_or(1)
        .max(1);

    let metrics = Metrics::new();
    let ctx = Ctx {
        timing_threshold,
        pad,
        metrics: &metrics,
    };

    let start_all = Instant::now();
    let mut rows_processed: i64 = 0;

    let result = do_work(
        &cli,
        &db,
        &work_mem_to_set,
        &expanded,
        &ctx,
        &mut rows_processed,
    );

    let label = match &result {
        Ok(()) => "n/a".to_string(),
        Err(err) => exception_label(err),
    };

    let duration = start_all.elapsed().as_secs_f64();
    let rps = if duration > 0.0 {
        (rows_processed as f64 / duration) as i64
    } else {
        0
    };
    ctx.format_measure(
        duration,
        "duration of process",
        &format!("{rows_processed} rows processed {rps} rows/sec"),
        "[TOTAL]\t",
    );
    metrics
        .total_duration
        .with_label_values(&[&label, VERSION])
        .set(duration);
    metrics
        .total_rows
        .with_label_values(&[&label, VERSION])
        .set(rows_processed as f64);

    if let Some(prom_file) = &cli.prom_file {
        metrics.write_prom_file(Path::new(prom_file))?;
    }

    result
}

#[allow(clippy::too_many_arguments)]
fn do_work(
    cli: &Cli,
    db: &DbConfig,
    work_mem_to_set: &str,
    expanded: &[String],
    ctx: &Ctx,
    rows_processed: &mut i64,
) -> Result<()> {
    debug!(
        "Connecting to database {} on {}:{} as {}…",
        db.dbname, db.host, db.port, db.user
    );

    let mut config = Client::configure();
    config
        .host(&db.host)
        .port(db.port)
        .user(&db.user)
        .dbname(&db.dbname);
    if let Some(pw) = &db.password {
        config.password(pw);
    }
    let mut client = config.connect(NoTls).with_context(|| {
        format!(
            "connecting to database {} on {}:{} as {}",
            db.dbname, db.host, db.port, db.user
        )
    })?;

    // Everything happens in a single transaction, at the requested isolation.
    let mut txn = match pg_isolation(cli.isolation_level) {
        None => client.transaction()?,
        Some(level) => client.build_transaction().isolation_level(level).start()?,
    };

    let mut main_timer = MainTimer::new(ctx);

    let work_mem = set_work_mem(&mut txn, work_mem_to_set)?;
    main_timer.measure(
        "connect_db",
        &format!(
            "Connected to database {} on {}:{} as {} work_mem: {work_mem}",
            db.dbname, db.host, db.port, db.user
        ),
    );

    let order = compute_dependencies(&mut txn, expanded, cli.ignore_unknown_tables)?;
    debug!(
        "Tables deps order: {:?}",
        order.iter().map(|d| &d.table_name).collect::<Vec<_>>()
    );

    let schemas_to_include: HashSet<String> = order
        .iter()
        .map(|d| {
            if d.schema_name.is_empty() {
                "public".to_string()
            } else {
                d.schema_name.clone()
            }
        })
        .collect();

    let constraints_to_defer = iter_constraints_to_defer(&mut txn, &schemas_to_include)?;
    main_timer.measure("compute_deps", "computing dependencies, will insert rows…");

    // Insert phase (dependency order), deferring matching UNIQUE constraints.
    let mut constraints_to_restore: Vec<Constraint> = Vec::new();
    for dep in &order {
        for constraint in &constraints_to_defer {
            if constraint.table_name == dep.table_name && constraint.table_schema == dep.schema_name
            {
                set_constraint_deferred(&mut txn, constraint, true, ctx)?;
                constraints_to_restore.insert(0, constraint.clone());
            }
        }
        *rows_processed += update_table_from_csv(
            &mut txn,
            dep,
            &dep.csv_file_path,
            &cli.csv_column_separator,
            cli.max_delete_percent_rows,
            ctx,
        )?;
    }
    main_timer.measure("insert_rows", "inserting rows, will delete rows…");

    // Delete phase (reverse dependency order).
    for dep in order.iter().rev() {
        delete_rows(&mut txn, dep, ctx)?;
    }
    main_timer.measure("delete_rows", "deleting rows, will commit");

    // Restore constraints to IMMEDIATE before commit.
    for constraint in &constraints_to_restore {
        set_constraint_deferred(&mut txn, constraint, false, ctx)?;
    }
    main_timer.measure(
        "reactivate_constraints",
        &format!(
            "re-enabling {} deferred constraints",
            constraints_to_restore.len()
        ),
    );

    // Pre-commit SQL.
    for sql in &cli.pre_commit_sql {
        process_sql_call(&mut txn, sql, ctx)?;
    }
    main_timer.measure(
        "run_pre_commit_functions",
        &format!("run pre-commit function: {:?}", cli.pre_commit_sql),
    );

    txn.commit()?;
    main_timer.measure("commit", "commit and pre-commit SQL statements");

    // Everything succeeded: reset the error gauge.
    ctx.metrics.errors.with_label_values(&["all"]).set(0.0);
    Ok(())
}
