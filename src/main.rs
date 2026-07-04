use std::process::ExitCode;

fn main() -> ExitCode {
    match csv_pg_injector::run::run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("[ERROR] {err:#}");
            ExitCode::FAILURE
        }
    }
}
