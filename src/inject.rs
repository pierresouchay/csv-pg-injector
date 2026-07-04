//! Core injection logic: temp-table staging, CSV streaming via `COPY`, upserts,
//! deletions, deferrable constraints and pre-commit SQL.

use std::collections::HashSet;
use std::io::{self, Write};
use std::time::Instant;

use anyhow::{bail, Context as _, Result};
use csv::{ReaderBuilder, WriterBuilder};

use crate::debug;
use crate::deps::Dep;
use crate::Ctx;

const SQL_FUNC_PREFIX: &str = "function ";

/// Column metadata as introspected from `INFORMATION_SCHEMA.COLUMNS`.
#[derive(Debug, Clone)]
pub struct ColumnInfo {
    pub schema_name: String,
    pub table_name: String,
    pub name: String,
    pub nullable: bool,
}

impl ColumnInfo {
    fn not_null(&self) -> bool {
        !self.nullable
    }
}

fn db_is_true(val: &str) -> Result<bool> {
    match val {
        "YES" => Ok(true),
        "NO" => Ok(false),
        other => bail!("Don't know how to make a bool Value for {other}"),
    }
}

/// Quote a `schema.table` (or bare table) name for SQL.
pub fn escape_table_name(schema: &str, table: &str) -> String {
    if schema.is_empty() {
        format!("\"{table}\"")
    } else {
        format!("\"{schema}\".\"{table}\"")
    }
}

/// The updatable, non-generated columns of a table, in ordinal order.
pub fn lookup_columns_from_database(
    txn: &mut postgres::Transaction,
    table_name: &str,
    schema_name: &str,
) -> Result<Vec<ColumnInfo>> {
    let query = "SELECT column_name, is_nullable FROM INFORMATION_SCHEMA.COLUMNS
                 WHERE table_name = $1 AND is_generated = 'NEVER' AND is_updatable = 'YES'
                   AND table_schema = $2
                 ORDER BY ORDINAL_POSITION";
    let mut columns = Vec::new();
    for row in txn.query(query, &[&table_name, &schema_name])? {
        let name: String = row.get(0);
        let is_nullable: String = row.get(1);
        columns.push(ColumnInfo {
            schema_name: schema_name.to_string(),
            table_name: table_name.to_string(),
            name,
            nullable: db_is_true(&is_nullable)?,
        });
    }
    Ok(columns)
}

/// Transform a raw CSV value into what is streamed to `COPY` (empty string means
/// SQL NULL). A not-null column with a missing value is an error.
fn dump_val(col: &ColumnInfo, v: Option<&str>) -> Result<String> {
    match v {
        None => {
            if col.not_null() {
                bail!(
                    "Cannot inject null value in column {}.{}.{}  [not null]",
                    col.schema_name,
                    col.table_name,
                    col.name
                );
            }
            Ok(String::new())
        }
        Some("NULL") | Some("") => Ok(String::new()),
        Some(s) => Ok(s.to_string()),
    }
}

/// A `std::io::Write` wrapper that counts the bytes written, used to report the
/// number of CSV bytes sent to PostgreSQL.
struct CountingWriter<W: Write> {
    inner: W,
    count: u64,
}

impl<W: Write> Write for CountingWriter<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let n = self.inner.write(buf)?;
        self.count += n as u64;
        Ok(n)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

/// sets the `rows_duration` gauge and optionally prints a timed line.
struct Measurer {
    start: Instant,
    table: String,
}

impl Measurer {
    fn new(table: &str) -> Self {
        Measurer {
            start: Instant::now(),
            table: table.to_string(),
        }
    }
    fn measure(&mut self, ctx: &Ctx, operation: &str, msg: &str) {
        let dur = self.start.elapsed().as_secs_f64();
        if !operation.is_empty() {
            ctx.metrics
                .rows_duration
                .with_label_values(&[&self.table, operation])
                .set(dur);
        }
        ctx.format_measure(dur, &self.table, msg, "\t");
        self.start = Instant::now();
    }
}

/// Perform all the upserts for a single table: stage the CSV in a temp table via
/// `COPY`, then `INSERT ... ON CONFLICT`. Returns the number of rows processed.
pub fn update_table_from_csv(
    txn: &mut postgres::Transaction,
    dep: &Dep,
    csv_file_path: &str,
    csv_column_separator: &str,
    max_delete_percent_rows: f64,
    ctx: &Ctx,
) -> Result<i64> {
    let mut measurer = Measurer::new(&dep.table_name);
    let db_columns = lookup_columns_from_database(txn, &dep.table_name, &dep.schema_name)?;
    let db_names: HashSet<&str> = db_columns.iter().map(|c| c.name.as_str()).collect();

    let separator = *csv_column_separator.as_bytes().first().unwrap_or(&b',');

    // Read the header to determine the columns present in both CSV and DB.
    let mut reader = ReaderBuilder::new()
        .delimiter(separator)
        .flexible(true)
        .from_path(csv_file_path)
        .with_context(|| format!("opening CSV file {csv_file_path}"))?;
    let header: Vec<String> = reader
        .headers()
        .with_context(|| format!("reading header of {csv_file_path}"))?
        .iter()
        .map(|s| s.to_string())
        .collect();

    // row_titles: CSV-header order, restricted to columns that exist in the DB.
    let row_titles: Vec<String> = header
        .iter()
        .filter(|h| db_names.contains(h.as_str()))
        .cloned()
        .collect();

    let ignored: Vec<&ColumnInfo> = db_columns
        .iter()
        .filter(|c| !row_titles.contains(&c.name))
        .collect();
    if !ignored.is_empty() {
        debug!(
            "\tIgnoring columns {:?} because not in {:?}",
            ignored.iter().map(|c| &c.name).collect::<Vec<_>>(),
            row_titles
        );
    }

    // columns_to_write: DB (ordinal) order, restricted to row_titles.
    let columns_to_write: Vec<&ColumnInfo> = db_columns
        .iter()
        .filter(|c| row_titles.contains(&c.name))
        .collect();
    let columns_to_write_names: Vec<String> =
        columns_to_write.iter().map(|c| c.name.clone()).collect();

    // Every primary key must be among the retained columns.
    for pk in &dep.pks {
        if !columns_to_write_names.contains(pk) {
            let mut sorted = columns_to_write_names.clone();
            sorted.sort();
            bail!(
                "Cannot inject {csv_file_path} because key column `{pk}` is not in the retained CSV columns: {}",
                sorted.join(",")
            );
        }
    }

    let table_escaped = escape_table_name(&dep.schema_name, &dep.table_name);
    let tmp_table = format!("tmp_{}", dep.table_name);
    let tmp_escaped = format!("\"{tmp_table}\"");
    let cols_join = columns_to_write_names.join(",");
    let pks_join = dep.pks.join(",");

    // Stage temp table (structure only) with an index on the primary keys.
    txn.execute(
        &format!(
            "CREATE TEMP TABLE {tmp_escaped} ON COMMIT DROP AS SELECT {cols_join} FROM {table_escaped} WITH NO DATA"
        ),
        &[],
    )?;
    // access method "hash" does not support multicolumn indexes
    let index_type = if dep.pks.len() > 1 { "" } else { "USING HASH" };
    txn.execute(
        &format!("CREATE INDEX ON {tmp_escaped} {index_type} ({pks_join})"),
        &[],
    )?;
    measurer.measure(
        ctx,
        "create_temp_table",
        &format!(
            "creating temp table with {} columns",
            columns_to_write.len()
        ),
    );

    // Row count of the destination table before we mutate anything.
    let db_row_count: i64 = txn
        .query_one(&format!("SELECT COUNT(*) FROM {table_escaped}"), &[])?
        .get(0);

    // Precompute, for each title, its position in the CSV record and its column.
    let title_plan: Vec<(usize, &ColumnInfo)> = row_titles
        .iter()
        .map(|t| {
            let idx = header
                .iter()
                .position(|h| h == t)
                .expect("title from header");
            let col = columns_to_write
                .iter()
                .copied()
                .find(|c| &c.name == t)
                .expect("title is a db column");
            (idx, col)
        })
        .collect();

    // Stream the CSV rows into the temp table via COPY.
    let mut row_count: i64 = 0;
    let mut bytes_sent: u64 = 0;
    if !row_titles.is_empty() {
        let quoted_titles = row_titles
            .iter()
            .map(|t| format!("\"{t}\""))
            .collect::<Vec<_>>()
            .join(", ");
        let copy_sql = format!(
            "COPY {tmp_escaped} ({quoted_titles}) FROM STDIN WITH (FORMAT CSV, DELIMITER E'\\t', ENCODING 'utf-8', NULL '')"
        );
        let sink = txn.copy_in(&copy_sql)?;
        let counting = CountingWriter {
            inner: sink,
            count: 0,
        };
        let mut csv_writer = WriterBuilder::new().delimiter(b'\t').from_writer(counting);

        let mut record = csv::StringRecord::new();
        let mut out: Vec<String> = Vec::with_capacity(title_plan.len());
        while reader.read_record(&mut record)? {
            out.clear();
            for (idx, col) in &title_plan {
                out.push(dump_val(col, record.get(*idx))?);
            }
            csv_writer.write_record(&out)?;
            row_count += 1;
        }
        csv_writer.flush()?;
        let counting = csv_writer
            .into_inner()
            .map_err(|e| anyhow::anyhow!("flushing COPY stream: {e}"))?;
        bytes_sent = counting.count;
        counting.inner.finish()?;
    }

    measurer.measure(
        ctx,
        "copy",
        &format!(
            "copying {row_count} rows into {} columns of temp table",
            row_titles.len()
        ),
    );

    // Validate the COPY landed exactly the rows we streamed.
    let tmp_row_count: i64 = txn
        .query_one(&format!("SELECT COUNT(*) FROM {tmp_escaped}"), &[])?
        .get(0);
    if tmp_row_count != row_count {
        bail!(
            "temp table row count {tmp_row_count} does not match streamed rows {row_count} for {}",
            dep.table_name
        );
    }

    // Deletion safety check, using the *real* CSV row count.
    let deletion_rate = if db_row_count == 0 {
        0.0
    } else {
        (100.0 * (db_row_count - row_count) as f64 / db_row_count as f64).max(0.0)
    };
    if deletion_rate > max_delete_percent_rows {
        bail!(
            "Delete threshold of {max_delete_percent_rows}% exceeded for table '{}'. \
             CSV rows: {row_count}, Database rows: {db_row_count}, Expected deletion rate: {deletion_rate:.2}%.",
            dep.table_name
        );
    }

    // Upsert from the temp table into the destination table.
    let columns_to_update: String = row_titles
        .iter()
        .filter(|t| !dep.pks.contains(t))
        .map(|c| format!("{c} = EXCLUDED.{c}"))
        .collect::<Vec<_>>()
        .join(",");
    let mut stmt =
        format!("INSERT INTO {table_escaped}({cols_join}) SELECT {cols_join} FROM {tmp_escaped} ");
    if columns_to_update.is_empty() {
        stmt.push_str("ON CONFLICT DO NOTHING;");
    } else {
        stmt.push_str(&format!(
            "ON CONFLICT({pks_join}) DO UPDATE SET {columns_to_update};"
        ));
    }

    if let Err(err) = txn.execute(stmt.as_str(), &[]) {
        let class = err
            .as_db_error()
            .map(|e| e.code().code().to_string())
            .unwrap_or_else(|| "Error".to_string());
        ctx.metrics
            .rows_added
            .with_label_values(&[&dep.table_name, &class])
            .set(row_count as f64);
        let msg = format!(
            "Failed inserting rows in {} with statement: {stmt}, error: {err}",
            dep.table_name
        );
        ctx.metrics.record_error("insert", &msg);
        return Err(err).context(msg);
    }

    ctx.metrics
        .rows_added
        .with_label_values(&[&dep.table_name, "n/a"])
        .set(row_count as f64);
    ctx.metrics
        .csv_file_size_bytes
        .with_label_values(&[&dep.table_name])
        .set(
            std::fs::metadata(csv_file_path)
                .map(|m| m.len())
                .unwrap_or(0) as f64,
        );
    ctx.metrics
        .csv_sent_pg_bytes
        .with_label_values(&[&dep.table_name])
        .set(bytes_sent as f64);

    measurer.measure(
        ctx,
        "insert",
        &format!(
            "inserting {row_count} values in {} columns on table {}",
            columns_to_write.len(),
            dep.table_name
        ),
    );

    Ok(row_count)
}

/// Delete rows in the destination table that are absent from the staged temp
/// table (obsolete rows), matched on the primary key.
pub fn delete_rows(txn: &mut postgres::Transaction, dep: &Dep, ctx: &Ctx) -> Result<()> {
    let mut measurer = Measurer::new(&dep.table_name);
    let tmp_escaped = format!("\"tmp_{}\"", dep.table_name);
    let table_escaped = escape_table_name(&dep.schema_name, &dep.table_name);
    let where_stmt = dep
        .pks
        .iter()
        .map(|k| format!("r.{k} = l.{k}"))
        .collect::<Vec<_>>()
        .join(" AND ");
    txn.execute(
        &format!(
            "DELETE FROM {table_escaped} l WHERE NOT EXISTS (SELECT NULL FROM {tmp_escaped} r WHERE {where_stmt})"
        ),
        &[],
    )?;
    measurer.measure(ctx, "delete", "deleting rows");
    Ok(())
}

/// A deferrable constraint that can be toggled DEFERRED / IMMEDIATE.
#[derive(Debug, Clone)]
pub struct Constraint {
    pub table_schema: String,
    pub table_name: String,
    pub constraint_name: String,
}

/// UNIQUE, deferrable-but-not-initially-deferred constraints in the given schemas.
pub fn iter_constraints_to_defer(
    txn: &mut postgres::Transaction,
    schemas_to_include: &HashSet<String>,
) -> Result<Vec<Constraint>> {
    if schemas_to_include.is_empty() {
        return Ok(Vec::new());
    }
    let list = schemas_to_include
        .iter()
        .map(|s| format!("'{s}'"))
        .collect::<Vec<_>>()
        .join(",");
    let query = format!(
        "SELECT table_schema, table_name, constraint_name FROM INFORMATION_SCHEMA.TABLE_CONSTRAINTS
         WHERE table_schema IN({list}) AND constraint_type IN ('UNIQUE')
           AND is_deferrable = 'YES' AND initially_deferred = 'NO'"
    );
    let mut constraints = Vec::new();
    for row in txn.query(&query, &[])? {
        constraints.push(Constraint {
            table_schema: row.get(0),
            table_name: row.get(1),
            constraint_name: row.get(2),
        });
    }
    Ok(constraints)
}

/// Set a constraint DEFERRED (true) or IMMEDIATE (false).
pub fn set_constraint_deferred(
    txn: &mut postgres::Transaction,
    constraint: &Constraint,
    deferred: bool,
    ctx: &Ctx,
) -> Result<()> {
    let arg = if deferred { "DEFERRED" } else { "IMMEDIATE" };
    if let Err(err) = txn.execute(
        &format!("SET CONSTRAINTS {} {arg}", constraint.constraint_name),
        &[],
    ) {
        ctx.metrics.record_error(
            if deferred {
                "constraints_disable"
            } else {
                "constraints_enable"
            },
            &format!(
                "Cannot modify constraint '{}' on table '{}': {err}",
                constraint.constraint_name, constraint.table_name
            ),
        );
        return Err(err.into());
    }
    Ok(())
}

/// Execute a `--pre-commit-sql` entry. A `function ` prefix calls the function
/// (`SELECT name()`); everything else is executed verbatim.
pub fn process_sql_call(txn: &mut postgres::Transaction, sql: &str, ctx: &Ctx) -> Result<()> {
    let (kind, statement) = if let Some(rest) = sql.strip_prefix(SQL_FUNC_PREFIX) {
        let mut name = rest.trim_end_matches(';');
        name = name.strip_suffix("()").unwrap_or(name);
        ("function", format!("SELECT {name}()"))
    } else {
        ("statement", sql.to_string())
    };
    if let Err(err) = txn.execute(statement.as_str(), &[]) {
        ctx.metrics.record_error(
            "pre_commmit_sql",
            &format!("failed to execute {kind} `{sql}`: {err}"),
        );
        return Err(err.into());
    }
    Ok(())
}

/// Show (and optionally set) `work_mem` for the connection.
pub fn set_work_mem(txn: &mut postgres::Transaction, work_mem_to_set: &str) -> Result<String> {
    let mut work_mem: String = txn.query_one("show work_mem", &[])?.get(0);
    debug!("Connection default work_mem is {work_mem}");
    if !work_mem_to_set.is_empty() {
        txn.execute(&format!("SET work_mem = '{work_mem_to_set}'"), &[])?;
        work_mem = txn.query_one("show work_mem", &[])?.get(0);
        debug!("Connection work_mem changed to {work_mem}");
    }
    Ok(work_mem)
}
