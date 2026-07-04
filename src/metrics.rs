//! Prometheus metrics

use std::fs;
use std::path::Path;

use anyhow::{Context, Result};
use prometheus::{Encoder, GaugeVec, Opts, Registry, TextEncoder};

pub const NAMESPACE: &str = "csv_pg_injector";

/// All the metrics exported by the tool, sharing a single registry.
pub struct Metrics {
    pub registry: Registry,
    /// Number of rows inserted, labelled by table and exception class.
    pub rows_added: GaugeVec,
    /// Time needed to perform a row operation, labelled by table and operation.
    pub rows_duration: GaugeVec,
    /// Bytes of the CSV source file, labelled by table.
    pub csv_file_size_bytes: GaugeVec,
    /// CSV bytes sent to PG after column filtering, labelled by table.
    pub csv_sent_pg_bytes: GaugeVec,
    /// Unrecoverable errors during the process, labelled by operation.
    pub errors: GaugeVec,
    /// Global duration in seconds, labelled by step.
    pub process_duration: GaugeVec,
    /// Total duration in seconds, labelled by exception class and version.
    pub total_duration: GaugeVec,
    /// Total number of rows processed, labelled by exception class and version.
    pub total_rows: GaugeVec,
}

fn gauge(registry: &Registry, name: &str, help: &str, labels: &[&str]) -> GaugeVec {
    let opts = Opts::new(name, help).namespace(NAMESPACE);
    let g = GaugeVec::new(opts, labels).expect("valid gauge definition");
    registry
        .register(Box::new(g.clone()))
        .expect("gauge registration");
    g
}

impl Metrics {
    pub fn new() -> Self {
        let registry = Registry::new();
        Metrics {
            rows_added: gauge(
                &registry,
                "rows_count",
                "Number of rows inserted",
                &["tablename", "exception"],
            ),
            rows_duration: gauge(
                &registry,
                "rows_duration_seconds",
                "Time needed to perform row operation",
                &["tablename", "operation"],
            ),
            csv_file_size_bytes: gauge(
                &registry,
                "csv_file_size_bytes",
                "Bytes of CSV source file",
                &["tablename"],
            ),
            csv_sent_pg_bytes: gauge(
                &registry,
                "csv_sent_pg_bytes",
                "CSV bytes sent to PG after column filtering and without header",
                &["tablename"],
            ),
            errors: gauge(
                &registry,
                "errors_during_process",
                "Unrecoverable errors during the process",
                &["operation"],
            ),
            process_duration: gauge(
                &registry,
                "global_duration_secs",
                "Global duration in seconds",
                &["step"],
            ),
            total_duration: gauge(
                &registry,
                "total_duration_seconds",
                "Total duration in seconds",
                &["exception", "version"],
            ),
            total_rows: gauge(
                &registry,
                "total_rows_count",
                "Total number of rows processed",
                &["exception", "version"],
            ),
            registry,
        }
    }

    /// Increment the error counter for the given operation and log to stderr.
    pub fn record_error(&self, operation: &str, message: &str) {
        self.errors.with_label_values(&[operation]).inc();
        eprintln!("[ERROR] {message}");
    }

    /// Write the metrics to a Prometheus textfile.
    pub fn write_prom_file(&self, path: &Path) -> Result<()> {
        let mut buffer = Vec::new();
        let encoder = TextEncoder::new();
        let families = self.registry.gather();
        encoder
            .encode(&families, &mut buffer)
            .context("encoding prometheus metrics")?;
        fs::write(path, buffer).with_context(|| format!("writing prom file {}", path.display()))?;
        Ok(())
    }
}

impl Default for Metrics {
    fn default() -> Self {
        Self::new()
    }
}
