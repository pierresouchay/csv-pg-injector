//! Parsing of `DATABASE_URL` (`postgresql://user:pass@host:port/dbname`).
use percent_encoding::percent_decode_str;
use regex::Regex;

/// Connection attributes extracted from a `DATABASE_URL`.
///
/// Every field is optional: only the components actually present in the URL are
/// set, so the result can be merged on top of defaults
#[derive(Debug, Default, PartialEq, Eq, Clone)]
pub struct DbUrl {
    pub host: Option<String>,
    pub dbname: Option<String>,
    pub user: Option<String>,
    pub password: Option<String>,
    pub port: Option<u16>,
}

fn decode(s: &str) -> String {
    percent_decode_str(s).decode_utf8_lossy().into_owned()
}

/// Parse a `postgresql://` URL into its components.
///
/// Anything that is not a `postgresql://` URL (empty string, `mysql://…`, …)
pub fn parse_database_url(db_url: &str) -> DbUrl {
    let re = Regex::new(r"^postgresql://(([^@]+)?@)?([^/:]*)(:([1-9][0-9]*))?/([^?]*)")
        .expect("static regex is valid");

    let caps = match re.captures(db_url) {
        Some(c) => c,
        None => return DbUrl::default(),
    };

    let mut res = DbUrl {
        host: Some(decode(caps.get(3).map_or("", |m| m.as_str()))),
        dbname: Some(decode(caps.get(6).map_or("", |m| m.as_str()))),
        ..DbUrl::default()
    };

    if let Some(userpass) = caps.get(2) {
        let mut parts = userpass.as_str().splitn(3, ':');
        if let Some(user) = parts.next() {
            res.user = Some(decode(user));
        }
        if let Some(password) = parts.next() {
            res.password = Some(decode(password));
        }
    }

    if let Some(port) = caps.get(5) {
        res.port = port.as_str().parse::<u16>().ok();
    }

    res
}
