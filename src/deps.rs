//! Table discovery and dependency ordering.
use std::collections::HashSet;
use std::fs;
use std::path::Path;

use anyhow::{bail, Result};
use postgres::types::ToSql;
use postgres::Transaction;

use crate::debug;

/// A table to inject, with its primary keys and foreign-key dependencies.
#[derive(Debug, Clone)]
pub struct Dep {
    pub csv_file_path: String,
    pub schema_name: String,
    pub table_name: String,
    pub pks: Vec<String>,
    /// Names of tables (within the working set) this table has a FK on.
    pub has_fk_on: HashSet<String>,
}

/// Strip the directory and the trailing `.csv` (last 4 chars) from a path
pub fn table_name_from_path(path: &str) -> String {
    let base = Path::new(path)
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    if base.len() >= 4 {
        base[..base.len() - 4].to_string()
    } else {
        base
    }
}

/// If a single directory is given, expand it into the `*.csv` files it contains.
pub fn expand_csv_files(csv_files: &[String]) -> Vec<String> {
    if csv_files.len() == 1 {
        let only = &csv_files[0];
        if Path::new(only).is_dir() {
            let mut expanded: Vec<String> = Vec::new();
            if let Ok(entries) = fs::read_dir(only) {
                for entry in entries.flatten() {
                    let p = entry.path();
                    if p.extension().and_then(|e| e.to_str()) == Some("csv") {
                        expanded.push(p.to_string_lossy().into_owned());
                    }
                }
            }
            println!(
                "Expanded csv files in dir {} found {} files",
                only,
                expanded.len()
            );
            return expanded;
        }
    }
    csv_files.to_vec()
}

fn placeholders(n: usize) -> String {
    (1..=n)
        .map(|i| format!("${i}"))
        .collect::<Vec<_>>()
        .join(",")
}

/// Build [`Dep`] objects for the given CSV files by introspecting the database
/// catalog for primary keys and foreign keys.
pub fn build_deps(txn: &mut Transaction, csv_files: &[String]) -> Result<Vec<Dep>> {
    // table_name -> csv path
    let mut table_to_path: Vec<(String, String)> = Vec::new();
    for path in csv_files {
        let name = table_name_from_path(path);
        if let Some(existing) = table_to_path.iter_mut().find(|(n, _)| *n == name) {
            existing.1 = path.clone();
        } else {
            table_to_path.push((name, path.clone()));
        }
    }
    if table_to_path.is_empty() {
        return Ok(Vec::new());
    }

    let table_names: Vec<String> = table_to_path.iter().map(|(n, _)| n.clone()).collect();
    let params: Vec<&(dyn ToSql + Sync)> = table_names
        .iter()
        .map(|s| s as &(dyn ToSql + Sync))
        .collect();
    let ph = placeholders(table_names.len());

    // Primary keys, ordered by (schema, table, ordinal position).
    let pk_query = format!(
        "SELECT tc.table_schema, tc.table_name, kcu.column_name
         FROM INFORMATION_SCHEMA.TABLE_CONSTRAINTS tc
         JOIN INFORMATION_SCHEMA.KEY_COLUMN_USAGE kcu
             ON tc.constraint_name = kcu.constraint_name
             AND tc.table_schema = kcu.table_schema
             AND tc.table_name = kcu.table_name
         WHERE tc.constraint_type = 'PRIMARY KEY'
             AND tc.table_name IN ({ph})
         ORDER BY tc.table_schema, tc.table_name, kcu.ordinal_position"
    );

    // Preserve insertion order of (schema, table) as rows arrive.
    let mut tables: Vec<Dep> = Vec::new();
    for row in txn.query(&pk_query, &params)? {
        let schema: String = row.get(0);
        let table: String = row.get(1);
        let column: String = row.get(2);
        if let Some(dep) = tables
            .iter_mut()
            .find(|d| d.schema_name == schema && d.table_name == table)
        {
            dep.pks.push(column);
        } else {
            let csv_file_path = table_to_path
                .iter()
                .find(|(n, _)| *n == table)
                .map(|(_, p)| p.clone())
                .unwrap_or_default();
            tables.push(Dep {
                csv_file_path,
                schema_name: schema,
                table_name: table,
                pks: vec![column],
                has_fk_on: HashSet::new(),
            });
        }
    }

    enrich_deps_with_foreign_keys(txn, &mut tables)?;
    Ok(tables)
}

/// Populate `has_fk_on` on each dep with the tables (within the set) it
/// references via a foreign key.
fn enrich_deps_with_foreign_keys(txn: &mut Transaction, deps: &mut [Dep]) -> Result<()> {
    if deps.is_empty() {
        return Ok(());
    }
    let query = "SELECT
            n1.nspname AS table_schema,
            c1.relname AS table_name,
            n2.nspname AS foreign_table_schema,
            c2.relname AS foreign_table_name
        FROM pg_constraint con
        JOIN pg_class c1 ON con.conrelid = c1.oid
        JOIN pg_namespace n1 ON c1.relnamespace = n1.oid
        JOIN pg_class c2 ON con.confrelid = c2.oid
        JOIN pg_namespace n2 ON c2.relnamespace = n2.oid
        WHERE con.contype = 'f'
        ORDER BY n1.nspname, c1.relname";

    // (schema, table) -> index into deps
    let index: Vec<(String, String)> = deps
        .iter()
        .map(|d| (d.schema_name.clone(), d.table_name.clone()))
        .collect();

    for row in txn.query(query, &[])? {
        let table_schema: String = row.get(0);
        let table_name: String = row.get(1);
        let ftable_schema: String = row.get(2);
        let ftable_name: String = row.get(3);

        let this = index
            .iter()
            .position(|(s, t)| *s == table_schema && *t == table_name);
        let foreign_known = index
            .iter()
            .any(|(s, t)| *s == ftable_schema && *t == ftable_name);
        if let (Some(i), true) = (this, foreign_known) {
            deps[i].has_fk_on.insert(ftable_name.clone());
            debug!("\t {ftable_name} <- {table_name} (FK)");
        }
    }
    Ok(())
}

/// Compute the insertion order (tables without dependencies first), erroring on
/// unknown tables unless `ignore_unknown_tables` is set.
pub fn compute_dependencies(
    txn: &mut Transaction,
    csv_files: &[String],
    ignore_unknown_tables: bool,
) -> Result<Vec<Dep>> {
    let deps = build_deps(txn, csv_files)?;

    let found_paths: HashSet<&str> = deps.iter().map(|d| d.csv_file_path.as_str()).collect();
    let not_found: Vec<&String> = csv_files
        .iter()
        .filter(|p| !found_paths.contains(p.as_str()))
        .collect();
    if !not_found.is_empty() {
        let joined = not_found
            .iter()
            .map(|s| s.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        if ignore_unknown_tables {
            println!("\t[WARN] The tables {joined} could not be found in the database");
        } else {
            bail!("Don't know how to identify table corresponding to {joined}");
        }
    }

    // Topological sort: append a table once every FK it references is already
    // placed (self references allowed).
    let mut remaining: Vec<Dep> = deps;
    let mut sorted: Vec<Dep> = Vec::new();
    while !remaining.is_empty() {
        let placed: HashSet<String> = sorted.iter().map(|d| d.table_name.clone()).collect();
        let mut next_round: Vec<Dep> = Vec::new();
        let mut progressed = false;
        for dep in remaining.into_iter() {
            let ready = dep
                .has_fk_on
                .iter()
                .all(|fk| placed.contains(fk) || *fk == dep.table_name);
            if ready {
                sorted.push(dep);
                progressed = true;
            } else {
                next_round.push(dep);
            }
        }
        remaining = next_round;
        if !progressed {
            // Cyclic FK dependencies: keep remaining order to avoid an infinite loop.
            sorted.append(&mut remaining);
        }
    }

    Ok(sorted)
}
