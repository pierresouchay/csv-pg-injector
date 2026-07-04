use csv_pg_injector::dburl::{parse_database_url, DbUrl};

fn db(host: &str, dbname: &str) -> DbUrl {
    DbUrl {
        host: Some(host.to_string()),
        dbname: Some(dbname.to_string()),
        ..DbUrl::default()
    }
}

#[test]
fn parses_host_and_db() {
    assert_eq!(
        parse_database_url("postgresql://localhost/db"),
        db("localhost", "db")
    );
}

#[test]
fn parses_port() {
    let mut expected = db("localhost", "db");
    expected.port = Some(789);
    assert_eq!(
        parse_database_url("postgresql://localhost:789/db"),
        expected
    );
}

#[test]
fn empty_userpass_before_at() {
    assert_eq!(
        parse_database_url("postgresql://@localhost/db"),
        db("localhost", "db")
    );
}

#[test]
fn parses_user_only() {
    let mut expected = db("localhost", "db");
    expected.user = Some("user".to_string());
    assert_eq!(
        parse_database_url("postgresql://user@localhost/db"),
        expected
    );
}

#[test]
fn parses_user_and_password() {
    let mut expected = db("localhost", "db");
    expected.user = Some("user".to_string());
    expected.password = Some("passwd".to_string());
    assert_eq!(
        parse_database_url("postgresql://user:passwd@localhost/db"),
        expected
    );
}

#[test]
fn parses_full_url() {
    let mut expected = db("localhost", "db");
    expected.user = Some("user".to_string());
    expected.password = Some("passwd".to_string());
    expected.port = Some(123);
    assert_eq!(
        parse_database_url("postgresql://user:passwd@localhost:123/db"),
        expected
    );
}

#[test]
fn percent_decodes_user_and_password() {
    let mut expected = db("localhost", "db");
    expected.user = Some("my\\user".to_string());
    expected.password = Some("My:fancyP@$$w0d!".to_string());
    expected.port = Some(123);
    assert_eq!(
        parse_database_url("postgresql://my%5Cuser:My%3AfancyP%40%24%24w0d%21@localhost:123/db"),
        expected
    );
}

#[test]
fn empty_string_yields_empty() {
    assert_eq!(parse_database_url(""), DbUrl::default());
}

#[test]
fn non_postgres_scheme_yields_empty() {
    assert_eq!(parse_database_url("mysql://localhost/db"), DbUrl::default());
}
