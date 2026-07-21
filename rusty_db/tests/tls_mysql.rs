#![cfg(feature = "mysql")]

//! Exercises encrypted (TLS) connections against a real MySQL/MariaDB
//! server.
//!
//! `rusty_db` has no TLS code of its own to test here — `MySqlDriver`
//! just hands the connection URL straight to sqlx, and `ssl-mode`/
//! `ssl-ca` are ordinary URL query parameters sqlx's MySQL driver already
//! understands (backed by `tls-rustls`, per this workspace's sqlx
//! features). What's actually being verified is that this passthrough
//! genuinely results in an encrypted session (via `SHOW STATUS LIKE
//! 'Ssl_cipher'`, not just "the connection didn't error"), and that
//! certificate verification really does reject a bad root rather than
//! silently accepting anything.
//!
//! There's no way to spin up a TLS-capable MySQL/MariaDB server portably
//! in every environment this suite runs in, so this is opt-in: point
//! `MYSQL_TEST_URL` at one, plus `MYSQL_TEST_CA_CERT` at its CA
//! certificate (this environment's MariaDB was set up with a
//! self-signed one at `/etc/mysql/cacert.pem` — its server cert has
//! `CN=127.0.0.1`, matching the host these tests connect to) — or the
//! tests skip themselves instead of failing when no server, or no
//! TLS-capable one, is reachable.

use rusty_db::mysql::MySqlDriver;
use rusty_db::Engine;

fn ca_cert_path() -> String {
    std::env::var("MYSQL_TEST_CA_CERT").unwrap_or_else(|_| "/etc/mysql/cacert.pem".to_string())
}

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
        "rusty_db_tls_mysql_wrong_ca_{}.pem",
        std::process::id()
    ));
    std::fs::write(&path, WRONG_CA_CERT).expect("failed to write scratch CA cert");
    path
}

fn base_url() -> String {
    std::env::var("MYSQL_TEST_URL")
        .unwrap_or_else(|_| "mysql://rusty:rusty@127.0.0.1/rusty_db_test".to_string())
}

/// Connects to a real MySQL/MariaDB server for this test with the given
/// extra query-string parameters. Returns `None` (test then skips
/// itself) rather than failing if no server, or no TLS-capable one, is
/// reachable.
async fn test_engine(query: &str) -> Option<Engine> {
    let url = format!("{}?{query}", base_url());
    match MySqlDriver::engine(&url).await {
        Ok(engine) => Some(engine),
        Err(err) => {
            eprintln!("skipping MySQL TLS test: could not connect to {url}: {err}");
            None
        }
    }
}

/// Whether *this session*'s connection actually negotiated TLS, straight
/// from MySQL/MariaDB's own status variables — not an assumption based
/// on which `ssl-mode` was requested. This is plain SQL (not the query
/// builder, which only covers DML) since it depends on a server status
/// variable.
async fn session_cipher(engine: &Engine) -> rusty_db::Result<String> {
    let row = engine
        .connect()
        .await?
        .fetch_one("SHOW STATUS LIKE 'Ssl_cipher'", &[])
        .await?;
    row.get_by_name::<String>("Value")
}

#[tokio::test]
async fn connections_use_tls_by_default_when_the_server_supports_it() -> rusty_db::Result<()> {
    let Some(engine) = test_engine("").await else {
        return Ok(());
    };
    assert!(
        !session_cipher(&engine).await?.is_empty(),
        "expected the default ssl-mode (preferred) to opportunistically use TLS \
         against a server that supports it"
    );
    Ok(())
}

#[tokio::test]
async fn ssl_mode_disabled_forces_a_plain_unencrypted_connection() -> rusty_db::Result<()> {
    let Some(engine) = test_engine("ssl-mode=DISABLED").await else {
        return Ok(());
    };
    assert!(
        session_cipher(&engine).await?.is_empty(),
        "expected ssl-mode=DISABLED to produce a plain, unencrypted connection"
    );
    Ok(())
}

#[tokio::test]
async fn ssl_mode_required_succeeds_and_encrypts_without_verifying_the_certificate(
) -> rusty_db::Result<()> {
    let Some(engine) = test_engine("ssl-mode=REQUIRED").await else {
        return Ok(());
    };
    assert!(!session_cipher(&engine).await?.is_empty());
    Ok(())
}

#[tokio::test]
async fn verify_ca_succeeds_with_the_servers_actual_ca_certificate() -> rusty_db::Result<()> {
    let query = format!("ssl-mode=VERIFY_CA&ssl-ca={}", ca_cert_path());
    let Some(engine) = test_engine(&query).await else {
        return Ok(());
    };
    assert!(!session_cipher(&engine).await?.is_empty());
    Ok(())
}

#[tokio::test]
async fn verify_ca_fails_when_the_root_cert_does_not_match_the_servers_certificate(
) -> rusty_db::Result<()> {
    // Only run this assertion once we know a *plain* connection to this
    // server works at all — otherwise a verify-ca failure could just
    // mean "no server reachable", which isn't what this test is about.
    if MySqlDriver::engine(&base_url()).await.is_err() {
        eprintln!(
            "skipping MySQL TLS test: no server reachable at {}",
            base_url()
        );
        return Ok(());
    }

    let wrong_ca = wrong_ca_path();
    let query = format!("ssl-mode=VERIFY_CA&ssl-ca={}", wrong_ca.display());

    let outcome = MySqlDriver::engine(&format!("{}?{query}", base_url())).await;
    assert!(
        outcome.is_err(),
        "expected VERIFY_CA to reject a root certificate that doesn't match the server's"
    );

    let _ = std::fs::remove_file(&wrong_ca);
    Ok(())
}
