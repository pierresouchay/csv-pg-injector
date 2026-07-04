use clap::{Parser, ValueEnum};

pub const DEFAULT_TIMING_THRESHOLD: f64 = 1.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum IsolationLevel {
    Default,
    Serializable,
    RepeatableRead,
    ReadCommitted,
    ReadUncommitted,
}

impl IsolationLevel {
    /// The SQL fragment used after `ISOLATION LEVEL`, or `None` for the
    /// server/session default (no explicit level requested).
    pub fn sql(self) -> Option<&'static str> {
        match self {
            IsolationLevel::Default => None,
            IsolationLevel::Serializable => Some("SERIALIZABLE"),
            IsolationLevel::RepeatableRead => Some("REPEATABLE READ"),
            IsolationLevel::ReadCommitted => Some("READ COMMITTED"),
            IsolationLevel::ReadUncommitted => Some("READ UNCOMMITTED"),
        }
    }
}

#[derive(Debug, Parser)]
#[command(
    name = "csv-pg-injector",
    version,
    about = "Inject CSV files into a PG DB by handling SQL dependencies between tables
    
    By default, it use env variable `$DATABASE_URL`
    
      with DATABASE_URL=postgresql://[[db_user]:[db_password]@]<db_host>[:db_port][/<db_name>]

    If `$DATABASE_URL` is missing in incomplete (missing password, user...), 
       Options can be used to complete (some of them also default on some well known PG env variables)
    "
)]
pub struct Cli {
    /// Database name (fallback to $PGDATABASE)
    #[arg(long = "db-name")]
    pub db_name: Option<String>,

    /// Database user (fallback to $PGUSER)
    #[arg(long = "db-user")]
    pub db_user: Option<String>,

    /// Database password (fallback to $PGPASSWORD)
    #[arg(long = "db-password")]
    pub db_password: Option<String>,

    /// Database hostname (fallback to $PGHOST or localhost)
    #[arg(long = "db-host")]
    pub db_host: Option<String>,

    /// Database port (defaults to 5432)
    #[arg(long = "db-port")]
    pub db_port: Option<u16>,

    #[arg(long = "csv-column-separator", default_value = ",")]
    pub csv_column_separator: String,

    /// Abort if deleting more than percent of rows
    #[arg(long = "max-delete-percent-rows", default_value_t = 100.0)]
    pub max_delete_percent_rows: f64,

    /// Only display operations taking more than this amount of secs
    #[arg(long = "timing-threshold", default_value_t = DEFAULT_TIMING_THRESHOLD)]
    pub timing_threshold: f64,

    /// prom file to write statistics to
    #[arg(long = "prom-file")]
    pub prom_file: Option<String>,

    #[arg(long = "ignore-unknown-tables", default_value_t = false)]
    pub ignore_unknown_tables: bool,

    /// SQL code(s) to execute before commit. Can be specified multiple times.
    /// Functions are prefixed by 'function '
    #[arg(long = "pre-commit-sql")]
    pub pre_commit_sql: Vec<String>,

    /// set isolation-level: https://www.postgresql.org/docs/current/transaction-iso.html
    #[arg(long = "isolation-level", value_enum, default_value_t = IsolationLevel::Default)]
    pub isolation_level: IsolationLevel,

    /// set work_mem (eg: 512MB, 1GB), put higher value to improve performance
    /// (default: $PG_WORK_MEM)
    #[arg(long = "work-mem")]
    pub work_mem: Option<String>,

    /// Single directory containing CSV files or list of CSV files to inject
    #[arg(required = true, num_args = 1..)]
    pub csv_files: Vec<String>,
}
