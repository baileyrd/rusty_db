#![cfg(feature = "postgres")]

//! Exercises encrypted (TLS) connections against a real Postgres server.
//!
//! `rusty_db` has no TLS code of its own to test here — `PostgresDriver`
//! just hands the connection URL straight to sqlx, and `sslmode`/
//! `sslrootcert` are ordinary URL query parameters sqlx's Postgres driver
//! already understands (backed by `tls-rustls`, per this workspace's sqlx
//! features). What's actually being verified is that this passthrough
//! genuinely results in an encrypted session (via Postgres's own
//! `pg_stat_ssl` catalog view, not just "the connection didn't error"),
//! and that certificate verification really does reject a bad root
//! rather than silently accepting anything.
//!
//! There's no way to spin up a TLS-capable Postgres server portably in
//! every environment this suite runs in, so this is opt-in: point
//! `POSTGRES_TEST_URL`/`POSTGRES_TEST_HOST` at one (see below) or the
//! tests skip themselves instead of failing when no server, or no
//! TLS-enabled server, is reachable.

use rusty_db::postgres::PostgresDriver;
use rusty_db::Engine;

/// This environment's Postgres (installed via the Debian/Ubuntu package)
/// generates a self-signed "snake oil" certificate for `localhost` and
/// enables `ssl = on` by default — no server-side setup was needed to
/// get *some* TLS support to test against. `sslmode=prefer` (the
/// client's own default) already opportunistically uses it.
const SNAKEOIL_CERT: &str = "/etc/ssl/certs/ssl-cert-snakeoil.pem";

/// A CA certificate that has nothing to do with the server's real one —
/// generated once for this test suite, not tied to any live server's
/// identity — used to prove that certificate verification actually
/// verifies something instead of accepting any well-formed CA.
const WRONG_CA_CERT: &str = "-----BEGIN CERTIFICATE-----
MIIDHzCCAgegAwIBAgIURnfia93SqEMUcjYMBJjHQSQslRYwDQYJKoZIhvcNAQEL
BQAwHzEdMBsGA1UEAwwUdG90YWxseV91bnJlbGF0ZWRfQ0EwHhcNMjYwNzIxMDQz
NDQ1WhcNMzYwNzE4MDQzNDQ1WjAfMR0wGwYDVQQDDBR0b3RhbGx5X3VucmVsYXRl
ZF9DQTCCASIwDQYJKoZIhvcNAQEBBQADggEPADCCAQoCggEBAM388W7mjRoolPVb
qNrPQVrNA4fG6kS3+A+Btur6PqOh6+Byv0QWkyhEYljeBuZKCZT/q0Uo1LLqwu7F
FJxdZPeUwNUZ9gcwPsIeo5TsqPXLG/BQpsEo98IX4v3E6ttSizDavJjxZ3LVWg2U
pA9nQxpjD2BmXdplX8bSjWQn3ISHVTJ/vKghc3LcMZ1cs4SqDtNGtyiLSF3alsGz
hGt8riqcoZEjZRZ586/lU56nbIoHEPC7Q0g8yqw009mAnftLj8EmKNmfohOollgK
0Do+nnXP87c4Padw04b+SsUnpSTJ+oJKr3Srynci3qTgHfOdo/m1/i+Ap+lChGge
shvar0sCAwEAAaNTMFEwHQYDVR0OBBYEFH/m+tcTvG4rGR0oN0QBv3vXe5e6MB8G
A1UdIwQYMBaAFH/m+tcTvG4rGR0oN0QBv3vXe5e6MA8GA1UdEwEB/wQFMAMBAf8w
DQYJKoZIhvcNAQELBQADggEBABczJKpxs5S+s5Q0MPpb4U1/5DaYdaw2mZzoCpx1
9jFcEY300Vqx7aET3COuB1bU7V8DZ3dS2bU9eu1Jr+QBUmtWNrzZ070r6zDXio2U
pR2Lb/O0I0Yo1+PISRP20+djJLMTLho9uGtmhQqZkVsm/Gs5dsmtEYpF8E3NbHbP
+AbvwR6lbE9kMJCqpc7reO0ajuRQVT0ATed9fvlT1runJrnq/t+BcxvNpZj40c0V
8yEOxehskGsFqIlAOQGY05Aubq9+vVPeIz19tlWKE91MBPBJs8kuZAOeqkMWU8H+
UaTbwEIRtQET8sD0MKQXZm78Wk5ugYfqJ4pyIxUGTNqkSf4=
-----END CERTIFICATE-----
";

fn wrong_ca_path() -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!(
        "rusty_db_tls_postgres_wrong_ca_{}.pem",
        std::process::id()
    ));
    std::fs::write(&path, WRONG_CA_CERT).expect("failed to write scratch CA cert");
    path
}

/// Connects to a real, reachable-over-TCP PostgreSQL server for this
/// test — `POSTGRES_TEST_HOST` (defaulting to `127.0.0.1`) lets a test
/// connect via `localhost` specifically, needed for hostname
/// verification (`sslmode=verify-full`) to match the snakeoil cert's
/// `CN=localhost`/`DNS:localhost`. Returns `None` (test then skips
/// itself) rather than failing if no server, or no TLS-capable one, is
/// reachable.
async fn test_engine_at(host: &str, query: &str) -> Option<Engine> {
    let base = std::env::var("POSTGRES_TEST_URL")
        .unwrap_or_else(|_| format!("postgres://rusty:rusty@{host}/rusty_db_test"));
    let url = format!("{base}?{query}");
    match PostgresDriver::engine(&url).await {
        Ok(engine) => Some(engine),
        Err(err) => {
            eprintln!("skipping Postgres TLS test: could not connect to {url}: {err}");
            None
        }
    }
}

/// Whether *this session*'s connection actually negotiated TLS, straight
/// from Postgres's own bookkeeping — not an assumption based on which
/// `sslmode` was requested. This is plain SQL (not the query builder,
/// which only covers DML) since it depends on a Postgres-specific system
/// view and function.
async fn session_is_encrypted(engine: &Engine) -> rusty_db::Result<bool> {
    let row = engine
        .connect()
        .await?
        .fetch_one(
            "SELECT ssl FROM pg_stat_ssl WHERE pid = pg_backend_pid()",
            &[],
        )
        .await?;
    row.get_by_name::<bool>("ssl")
}

#[tokio::test]
async fn connections_use_tls_by_default_when_the_server_supports_it() -> rusty_db::Result<()> {
    let Some(engine) = test_engine_at("127.0.0.1", "").await else {
        return Ok(());
    };
    assert!(
        session_is_encrypted(&engine).await?,
        "expected the default sslmode (prefer) to opportunistically use TLS \
         against a server that supports it"
    );
    Ok(())
}

#[tokio::test]
async fn sslmode_disable_forces_a_plain_unencrypted_connection() -> rusty_db::Result<()> {
    let Some(engine) = test_engine_at("127.0.0.1", "sslmode=disable").await else {
        return Ok(());
    };
    assert!(
        !session_is_encrypted(&engine).await?,
        "expected sslmode=disable to produce a plain, unencrypted connection"
    );
    Ok(())
}

#[tokio::test]
async fn sslmode_require_succeeds_and_encrypts_without_verifying_the_certificate(
) -> rusty_db::Result<()> {
    let Some(engine) = test_engine_at("127.0.0.1", "sslmode=require").await else {
        return Ok(());
    };
    assert!(session_is_encrypted(&engine).await?);
    Ok(())
}

#[tokio::test]
async fn verify_full_succeeds_with_the_servers_actual_certificate_and_matching_host(
) -> rusty_db::Result<()> {
    let query = format!("sslmode=verify-full&sslrootcert={SNAKEOIL_CERT}");
    // The snakeoil cert's CN/SAN is `localhost`, so hostname verification
    // needs to connect via that name specifically, not `127.0.0.1`.
    let Some(engine) = test_engine_at("localhost", &query).await else {
        return Ok(());
    };
    assert!(session_is_encrypted(&engine).await?);
    Ok(())
}

#[tokio::test]
async fn verify_ca_fails_when_the_root_cert_does_not_match_the_servers_certificate(
) -> rusty_db::Result<()> {
    let wrong_ca = wrong_ca_path();
    let query = format!("sslmode=verify-ca&sslrootcert={}", wrong_ca.display());

    let url = std::env::var("POSTGRES_TEST_URL")
        .unwrap_or_else(|_| "postgres://rusty:rusty@127.0.0.1/rusty_db_test".to_string());
    // Only run this assertion once we know a *plain* connection to this
    // server works at all — otherwise a verify-ca failure could just
    // mean "no server reachable", which isn't what this test is about.
    if PostgresDriver::engine(&url).await.is_err() {
        eprintln!("skipping Postgres TLS test: no server reachable at {url}");
        return Ok(());
    }

    let outcome = PostgresDriver::engine(&format!("{url}?{query}")).await;
    assert!(
        outcome.is_err(),
        "expected verify-ca to reject a root certificate that doesn't match the server's"
    );

    let _ = std::fs::remove_file(&wrong_ca);
    Ok(())
}
