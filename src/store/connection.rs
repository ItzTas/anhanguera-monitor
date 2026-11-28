use std::time::Duration;

// The url crate's type, the one sqlx parses addresses with.
use reqwest::Url;
use sea_orm::sqlx::ConnectOptions as _;
use sea_orm::sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use sea_orm::{DatabaseConnection, SqlxPostgresConnector};

use super::StoreError;

pub(crate) const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const NOT_A_POSTGRES_ADDRESS: &str = "DATABASE_URL is not a valid Postgres address";
const NO_PASSWORD: &str = "DATABASE_URL has no password: add the role's password to the URL";
pub(crate) const POOL_SIZE: u32 = 4;
pub(crate) const POOL_LIMITS: Limits = Limits {
    statement: Duration::from_secs(20),
    lock: Duration::from_secs(5),
};

#[derive(Clone, Copy)]
pub(crate) struct Limits {
    pub(crate) statement: Duration,
    pub(crate) lock: Duration,
}

pub(crate) fn database_options(database_url: &str) -> Result<PgConnectOptions, StoreError> {
    let documented = ["postgres://", "postgresql://"]
        .into_iter()
        .any(|scheme| database_url.starts_with(scheme));
    if !documented {
        return Err(StoreError::InvalidUrl(NOT_A_POSTGRES_ADDRESS));
    }
    let url =
        Url::parse(database_url).map_err(|_| StoreError::InvalidUrl(NOT_A_POSTGRES_ADDRESS))?;
    // Without a host, sqlx would pick one from PGHOST, a local socket or localhost.
    let host_in_query = url
        .query_pairs()
        .any(|(key, value)| (key == "host" || key == "hostaddr") && !value.is_empty());
    if url.host_str().is_none_or(str::is_empty) && !host_in_query {
        return Err(StoreError::InvalidUrl(NOT_A_POSTGRES_ADDRESS));
    }
    // Without a password, sqlx takes one from PGPASSWORD or .pgpass, and it logs a malformed
    // .pgpass line whole, password included.
    let password_in_query = url
        .query_pairs()
        .any(|(key, value)| key == "password" && !value.is_empty());
    if url.password().is_none_or(str::is_empty) && !password_in_query {
        return Err(StoreError::InvalidUrl(NO_PASSWORD));
    }
    // The url crate keeps an IPv6 host in brackets, and sqlx would look "[::1]" up as a name.
    let bare_ipv6 = url
        .host_str()
        .and_then(|host| host.strip_prefix('[')?.strip_suffix(']'))
        .filter(|_| !host_in_query)
        .map(str::to_owned);
    only_keys_sqlx_reads(&url)?;
    let mut options = PgConnectOptions::from_url(&url)
        .map_err(|_| StoreError::InvalidUrl(NOT_A_POSTGRES_ADDRESS))?;
    if let Some(address) = bare_ipv6 {
        options = options.host(&address);
    }
    if options.get_host().contains(',') {
        return Err(StoreError::InvalidUrl(NOT_A_POSTGRES_ADDRESS));
    }
    // sqlx sets no TCP keepalive of its own; these let the server drop a monitor that vanished.
    let server_keepalives = [
        ("tcp_keepalives_idle", 30),
        ("tcp_keepalives_interval", 10),
        ("tcp_keepalives_count", 3),
        ("tcp_user_timeout", 60_000),
    ];
    Ok(options
        .disable_statement_logging()
        .options(server_keepalives))
}

// sqlx logs every query key it does not read, value included, so any other key is refused.
fn only_keys_sqlx_reads(url: &Url) -> Result<(), StoreError> {
    let read = [
        "sslmode",
        "ssl-mode",
        "sslrootcert",
        "ssl-root-cert",
        "ssl-ca",
        "sslcert",
        "ssl-cert",
        "sslkey",
        "ssl-key",
        "statement-cache-capacity",
        "host",
        "hostaddr",
        "port",
        "dbname",
        "user",
        "password",
        "application_name",
        "options",
    ];
    let read_by_sqlx =
        |key: &str| read.contains(&key) || (key.starts_with("options[") && key.ends_with(']'));
    if url.query_pairs().any(|(key, _)| !read_by_sqlx(&key)) {
        return Err(StoreError::InvalidUrl(NOT_A_POSTGRES_ADDRESS));
    }
    Ok(())
}

pub(crate) fn with_limits(options: PgConnectOptions, limits: Limits) -> PgConnectOptions {
    options.options([
        ("statement_timeout", limits.statement.as_millis()),
        ("lock_timeout", limits.lock.as_millis()),
    ])
}

pub(crate) fn pool(options: PgConnectOptions) -> DatabaseConnection {
    // A minimum, an idle limit or a lifetime makes sqlx spawn a maintenance task, and
    // `Store::new` has to work outside a runtime.
    let pool = PgPoolOptions::new()
        .max_connections(POOL_SIZE)
        .min_connections(0)
        .acquire_timeout(CONNECT_TIMEOUT)
        .idle_timeout(None)
        .max_lifetime(None)
        .test_before_acquire(true)
        .connect_lazy_with(options);
    SqlxPostgresConnector::from_sqlx_postgres_pool(pool)
}
