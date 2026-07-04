# csv-pg-injector

Bulk-loads CSV files into PostgreSQL
with good performance while maintaining referential integrity between tables.

The schema (primary keys, columns, foreign keys, deferrable constraints) is
introspected **live from the PostgreSQL catalog**.

## Key features

- Uses PostgreSQL's native CSV parser (`COPY`) for fast bulk inserts
- Performs upserts (insert or update) using `ON CONFLICT` clauses
- Handles table dependencies automatically based on foreign-key relationships
- Runs the whole load in a single transaction for atomicity
- Exports Prometheus metrics (optional)
- Safety features such as a max delete-percentage limit
- Supports running SQL (e.g. refreshing materialized views) before commit

## Workflow

1. Inserts data in dependency order (tables without foreign keys first)
2. Uses temporary staging tables for the CSV data
3. Performs upserts to handle existing records
4. Deletes obsolete records in reverse dependency order
5. Runs `--pre-commit-sql` statements
6. Commits the data in a single transaction

## Usage

```
csv-pg-injector [OPTIONS] <csv_files>...
```

`<csv_files>` is either a single directory (all `*.csv` inside are loaded — handy
in a shell-less container) or an explicit list of CSV files. Each file maps to a
table named after the file (minus the `.csv` extension).

### Configuring the database

1. Set `DATABASE_URL=postgresql://<user>:<password>@<host>:<port>/<db_name>`
2. Override individual parameters with the `--db-*` flags (they win over the URL)

```bash
export DATABASE_URL=postgresql://db_user:secret@localhost/my_db
csv-pg-injector directory/containing/csv/files
```

### Options & command line help:

Run `csv-pg-injector --help`

```
Inject CSV files into a PG DB by handling SQL dependencies between tables
    
    By default, it use env variable `$DATABASE_URL`
    
      with DATABASE_URL=postgresql://[[db_user]:[db_password]@]<db_host>[:db_port][/<db_name>]

    If `$DATABASE_URL` is missing in incomplete (missing password, user...), 
       Options can be used to complete (some of them also default on some well known PG env variables)
    

Usage: csv-pg-injector [OPTIONS] <CSV_FILES>...

Arguments:
  <CSV_FILES>...  Single directory containing CSV files or list of CSV files to inject

Options:
      --db-name <DB_NAME>
          Database name (fallback to $PGDATABASE)
      --db-user <DB_USER>
          Database user (fallback to $PGUSER)
      --db-password <DB_PASSWORD>
          Database password (fallback to $PGPASSWORD)
      --db-host <DB_HOST>
          Database hostname (fallback to $PGHOST or localhost)
      --db-port <DB_PORT>
          Database port (defaults to 5432)
      --csv-column-separator <CSV_COLUMN_SEPARATOR>
          [default: ,]
      --max-delete-percent-rows <MAX_DELETE_PERCENT_ROWS>
          Abort if deleting more than percent of rows [default: 100]
      --timing-threshold <TIMING_THRESHOLD>
          Only display operations taking more than this amount of secs [default: 1]
      --prom-file <PROM_FILE>
          prom file to write statistics to
      --ignore-unknown-tables
          
      --pre-commit-sql <PRE_COMMIT_SQL>
          SQL code(s) to execute before commit. Can be specified multiple times. Functions are prefixed by 'function '
      --isolation-level <ISOLATION_LEVEL>
          set isolation-level: https://www.postgresql.org/docs/current/transaction-iso.html [default: default] [possible values: default, serializable, repeatable-read, read-committed, read-uncommitted]
      --work-mem <WORK_MEM>
          set work_mem (eg: 512MB, 1GB), put higher value to improve performance (default: $PG_WORK_MEM)
  -h, --help
          Print help
  -V, --version
          Print version
```

## Build & test

```bash
cargo build --release
cargo clippy --all-targets
```

Test & Integration tests:

```bash
./run_test.sh
```

## Creating a new version

Bump `version` in `Cargo.toml`, update `CHANGELOG.md`, then tag `vX.Y.Z`.
