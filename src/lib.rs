//! `csv-pg-injector` — bulk-load CSV files into PostgreSQL with referential
//! integrity, upserts and transaction safety.

pub mod cli;
pub mod dburl;
pub mod deps;
pub mod inject;
pub mod metrics;
pub mod run;

use metrics::Metrics;

/// True when the `DEBUG` environment variable is set to anything but `false`.
pub fn debug_enabled() -> bool {
    match std::env::var("DEBUG") {
        Ok(v) => v != "false",
        Err(_) => false,
    }
}

/// Print a `[DEBUG]` line, but only when [`debug_enabled`] is true.
#[macro_export]
macro_rules! debug {
    ($($arg:tt)*) => {
        if $crate::debug_enabled() {
            println!("[DEBUG] {}", format!($($arg)*));
        }
    };
}

/// Shared context threaded through the injection functions: timing/formatting
/// configuration and the metrics registry.
pub struct Ctx<'a> {
    pub timing_threshold: f64,
    pub pad: usize,
    pub metrics: &'a Metrics,
}

impl<'a> Ctx<'a> {
    /// Print a measurement line when the duration exceeds the timing threshold
    pub fn format_measure(&self, duration_secs: f64, table_name: &str, extra: &str, prefix: &str) {
        if duration_secs > self.timing_threshold {
            println!(
                "{prefix}{:>5.1} secs {:<pad$}\t{extra}",
                duration_secs,
                table_name,
                pad = self.pad
            );
        }
    }
}
