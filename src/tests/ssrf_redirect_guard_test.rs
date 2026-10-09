//! Regression (#146 / CRA-5): the SSRF guard must validate resolved addresses
//! on every connection, not only on the initial URL.
//!
//! The redirect-policy closure is synchronous, so it can only inspect a hop's
//! URL; it cannot resolve the hop's hostname. Before this fix a public fetch
//! that 302s to `internal.example` passed every check the closure could make and
//! then connected to whatever that name resolved to. These tests pin each half:
//!
//! * the shared verdict refuses a private or link-local answer, and refuses an
//!   empty answer rather than reading it as approval,
//! * the resolver itself refuses an internal answer and approves a public one,
//! * a client carrying the guard refuses a hostname whose answer is internal
//!   even though the URL itself is unremarkable (the rebinding shape),
//! * a redirect to a literal internal address is stopped by the policy,
//! * a redirect to a hostname that resolves internally is stopped by the
//!   resolver, which is the case the policy structurally cannot see,
//! * loopback and ordinary public redirects still work, so the guard is not a
//!   blanket block.
//!
//! Every server here is a loopback listener, so nothing in this file depends on
//! external DNS or on what the machine's resolver happens to answer.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

use crate::brain::tools::ssrf::{
    GuardedResolver, Lookup, LookupFuture, SystemLookup, guard_client, guard_resolved_addrs,
    redirect_policy,
};

/// Every client in this file is bounded, so a guard that failed to fail would
/// surface as a test timeout rather than a hung CI job.
const CLIENT_TIMEOUT: Duration = Duration::from_secs(10);

/// The full `Display` + `source()` chain, because reqwest wraps a resolver
/// failure inside a connect error and the guard's message is not the top one.
fn error_chain(err: &dyn std::error::Error) -> String {
    let mut out = err.to_string();
    let mut cursor = err.source();
    while let Some(next) = cursor {
        out.push_str(" | ");
        out.push_str(&next.to_string());
        cursor = next.source();
    }
    out
}

/// Serve one canned HTTP response on a loopback port, for as long as the test
/// needs it, and hand back the base URL.
async fn spawn_server(response: String) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("local addr");
    tokio::spawn(async move {
        while let Ok((mut socket, _)) = listener.accept().await {
            let body = response.clone();
            tokio::spawn(async move {
                let mut buf = [0u8; 1024];
                let _ = socket.read(&mut buf).await;
                let _ = socket.write_all(body.as_bytes()).await;
                let _ = socket.shutdown().await;
            });
        }
    });
    format!("http://{addr}/")
}

/// A 302 to `location`, with no body.
async fn spawn_redirect(location: &str) -> String {
    let response = format!(
        "HTTP/1.1 302 Found\r\n\
         Location: {location}\r\n\
         Content-Length: 0\r\n\
         Connection: close\r\n\r\n"
    );
    spawn_server(response).await
}

/// A 200 with `body`.
async fn spawn_ok(body: &str) -> String {
    spawn_server(format!(
        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    ))
    .await
}

/// Answers `rebind.test` with a link-local address, and defers to the system
/// resolver for every other name.
struct RebindingLookup;

impl Lookup for RebindingLookup {
    fn lookup(&self, host: &str) -> LookupFuture {
        if host == "rebind.test" {
            return Box::pin(async { Ok(vec![SocketAddr::from(([169, 254, 169, 254], 80))]) });
        }
        SystemLookup.lookup(host)
    }
}

/// Always answers with one public address, so the approve path is tested
/// without touching the network.
struct PublicLookup;

impl Lookup for PublicLookup {
    fn lookup(&self, _host: &str) -> LookupFuture {
        Box::pin(async { Ok(vec![SocketAddr::from(([93, 184, 216, 34], 443))]) })
    }
}

/// A client whose DNS answers come from `lookup`, with the same redirect policy
/// production uses.
fn client_with_lookup(lookup: Arc<dyn Lookup>) -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(CLIENT_TIMEOUT)
        .dns_resolver(GuardedResolver::with_lookup(lookup))
        .redirect(redirect_policy(10))
        .build()
        .expect("build client")
}

fn addr(s: &str) -> SocketAddr {
    s.parse().expect("socket addr")
}

// ---------------------------------------------------------------- the verdict

#[test]
fn a_public_answer_is_approved() {
    assert!(guard_resolved_addrs("example.com", &[addr("93.184.216.34:443")]).is_ok());
}

#[test]
fn a_loopback_answer_is_approved() {
    // The maintainer decision: loopback is reachable on purpose.
    assert!(guard_resolved_addrs("localhost", &[addr("127.0.0.1:80")]).is_ok());
    assert!(guard_resolved_addrs("localhost", &[addr("[::1]:80")]).is_ok());
}

#[test]
fn a_private_answer_is_refused_even_under_a_public_name() {
    let err = guard_resolved_addrs("internal.example", &[addr("10.0.0.5:80")])
        .expect_err("RFC1918 must be refused");
    assert!(err.contains("private"), "{err}");
    assert!(err.contains("SSRF guard"), "{err}");
}

#[test]
fn a_link_local_answer_is_refused() {
    let err = guard_resolved_addrs("internal.example", &[addr("169.254.169.254:80")])
        .expect_err("link-local must be refused");
    assert!(err.contains("link-local"), "{err}");
}

#[test]
fn one_bad_address_condemns_the_whole_answer() {
    // A rebinding answer can mix a public address with an internal one; taking
    // the first would be a coin flip.
    let err = guard_resolved_addrs(
        "internal.example",
        &[addr("93.184.216.34:443"), addr("192.168.1.10:443")],
    )
    .expect_err("a mixed answer must be refused");
    assert!(err.contains("private"), "{err}");
}

#[test]
fn an_empty_answer_is_refused_not_approved() {
    let err =
        guard_resolved_addrs("nowhere.example", &[]).expect_err("an empty answer is not approval");
    assert!(err.contains("no addresses"), "{err}");
}

#[test]
fn the_metadata_hostname_is_refused_before_dns() {
    let err = guard_resolved_addrs("metadata.google.internal", &[addr("93.184.216.34:80")])
        .expect_err("the metadata host must be refused by name");
    assert!(err.contains("metadata"), "{err}");
}

// ------------------------------------------------------ the resolver, direct

#[tokio::test]
async fn the_resolver_refuses_an_internal_answer() {
    let resolver = GuardedResolver::with_lookup(Arc::new(RebindingLookup));
    let err = resolver
        .resolve_guarded("rebind.test")
        .await
        .expect_err("a link-local answer must be refused");
    assert!(err.contains("link-local"), "{err}");
    assert!(err.contains("SSRF guard"), "{err}");
}

#[tokio::test]
async fn the_resolver_approves_and_returns_a_public_answer() {
    let resolver = GuardedResolver::with_lookup(Arc::new(PublicLookup));
    let addrs = resolver
        .resolve_guarded("example.com")
        .await
        .expect("a public answer is approved");
    assert_eq!(
        addrs,
        vec![addr("93.184.216.34:443")],
        "the resolver must hand over the very addresses it approved"
    );
}

// ------------------------------------------------------- the resolver, wired

#[tokio::test]
async fn a_hostname_resolving_internally_is_refused_at_connect_time() {
    // The URL is unremarkable and passes validate_url; only the resolver can
    // see where it actually points. This is the rebinding shape.
    let client = client_with_lookup(Arc::new(RebindingLookup));

    let err = client
        .get("http://rebind.test/")
        .send()
        .await
        .expect_err("a hostname resolving to link-local must not connect");

    assert!(err.is_connect(), "expected a connect failure, got {err}");
    let chain = error_chain(&err);
    assert!(chain.contains("SSRF guard"), "{chain}");
    assert!(chain.contains("link-local"), "{chain}");
}

#[tokio::test]
async fn loopback_still_works_through_the_guarded_resolver() {
    let base = spawn_ok("pong").await;
    let client = client_with_lookup(Arc::new(RebindingLookup));

    let resp = client
        .get(&base)
        .send()
        .await
        .expect("a loopback fetch must still succeed");
    assert_eq!(resp.status(), 200);
    assert_eq!(resp.text().await.expect("body"), "pong");
}

// --------------------------------------------------------- the redirect hops

#[tokio::test]
async fn a_redirect_to_a_literal_internal_address_is_stopped() {
    let base = spawn_redirect("http://169.254.169.254/latest/meta-data/").await;
    let client = client_with_lookup(Arc::new(RebindingLookup));

    let err = client
        .get(&base)
        .send()
        .await
        .expect_err("a hop to the metadata address must be blocked");

    let chain = error_chain(&err);
    assert!(chain.contains("redirect blocked by SSRF guard"), "{chain}");
}

#[tokio::test]
async fn a_redirect_to_a_hostname_resolving_internally_is_stopped() {
    // The case the synchronous closure cannot see: the hop's URL is a plain
    // hostname, so validate_url approves it and the resolver has to catch it.
    let base = spawn_redirect("http://rebind.test/").await;
    let client = client_with_lookup(Arc::new(RebindingLookup));

    let err = client
        .get(&base)
        .send()
        .await
        .expect_err("a hop resolving to link-local must be blocked");

    let chain = error_chain(&err);
    assert!(chain.contains("SSRF guard"), "{chain}");
    assert!(chain.contains("link-local"), "{chain}");
}

#[tokio::test]
async fn a_redirect_to_a_public_host_is_still_followed() {
    // The guard must not break ordinary redirects.
    let target = spawn_ok("landed").await;
    let base = spawn_redirect(&target).await;
    let client = client_with_lookup(Arc::new(RebindingLookup));

    let resp = client.get(&base).send().await.expect("public hop follows");
    assert_eq!(resp.status(), 200);
    assert_eq!(resp.text().await.expect("body"), "landed");
}

// ------------------------------------------------------ the production wiring

#[tokio::test]
async fn guard_client_still_serves_ordinary_requests() {
    // `guard_client` installs the real system resolver; this is the smoke test
    // that the wiring did not break the happy path.
    let base = spawn_ok("ok").await;
    let client = guard_client(reqwest::Client::builder().timeout(CLIENT_TIMEOUT), true)
        .build()
        .expect("build guarded client");

    let resp = client.get(&base).send().await.expect("loopback fetch");
    assert_eq!(resp.status(), 200);
}

#[tokio::test]
async fn guard_client_without_redirects_still_validates_the_first_request() {
    let base = spawn_redirect("http://169.254.169.254/").await;
    let client = guard_client(reqwest::Client::builder().timeout(CLIENT_TIMEOUT), false)
        .build()
        .expect("build guarded client");

    let resp = client
        .get(&base)
        .send()
        .await
        .expect("the first request itself is allowed");
    assert_eq!(
        resp.status(),
        302,
        "with redirects off the hop is not followed, but the fetch is still validated"
    );
}
