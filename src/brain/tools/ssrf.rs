//! Shared SSRF guard for every server-side fetch tool (OC-04).
//!
//! `web_scrape`, `http_request`, dynamic HTTP tools, and `browser_navigate` all
//! fetch a caller-influenced URL from the host's network, which is a classic
//! SSRF primitive: point one at `http://169.254.169.254/` (cloud metadata),
//! or an RFC1918 admin panel and it reaches
//! inside. This is the single guard they share so they cannot disagree.
//!
//! Three layers, and only the third one closes the redirect hole:
//!
//! * [`validate_url`] is a synchronous structural + literal-IP check, usable in
//!   a redirect-policy closure.
//! * [`validate_url_resolved`] adds a DNS resolution pass so a public-looking
//!   hostname that resolves to an internal address (`metadata.google.internal`,
//!   `169.254.169.254.nip.io`) is refused before the request goes out.
//! * [`GuardedResolver`] replaces reqwest's DNS resolver, so EVERY connection
//!   the client makes, redirect hops included, resolves, validates, and then
//!   connects to the exact addresses it just approved.
//!
//! The third layer is load-bearing. A redirect policy closure is synchronous, so
//! it can only see the hop's URL; it cannot resolve the hop's hostname. Validating
//! the initial URL alone therefore leaves a hole: an approved public fetch that
//! 302s to `internal.example` passes every check the closure can make and then
//! connects to whatever that name resolves to. Resolving inside the resolver
//! closes it, and because the addresses returned there are the ones hyper
//! connects to, there is no window between "validated" and "connected" for a
//! DNS-rebinding answer to be swapped in.
//!
//! Host classification goes through [`url::Host`] rather than `host_str()`,
//! which keeps the brackets on IPv6 literals and would let `[::1]` slip past.

use std::future::Future;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::pin::Pin;
use std::sync::Arc;

use url::{Host, Url};

/// Hostnames that name a cloud metadata endpoint by name rather than IP.
const METADATA_HOSTS: &[&str] = &["metadata.google.internal", "metadata", "metadata.goog"];

/// The reason `ip` must not be fetched, or None when it is a public address.
///
/// IPv4-mapped IPv6 (`::ffff:a.b.c.d`) is unmapped and re-checked as IPv4, so a
/// mapped metadata or loopback address cannot dodge the v4 rules.
pub(crate) fn forbidden_ip(ip: IpAddr) -> Option<&'static str> {
    match ip {
        IpAddr::V4(v4) => forbidden_v4(v4),
        IpAddr::V6(v6) => {
            if let Some(v4) = v6.to_ipv4_mapped() {
                return forbidden_v4(v4);
            }
            forbidden_v6(v6)
        }
    }
}

fn forbidden_v4(v4: Ipv4Addr) -> Option<&'static str> {
    // Loopback (127.0.0.0/8) is deliberately allowed (maintainer decision):
    // a local-first agent legitimately reaches the operator's own machine, and
    // the guest-driver path that made localhost debug ports an exfil target is
    // closed by the approval owner-gating (OC-01) and deny-by-default ingest
    // (OC-02). The SSRF teeth here are reaching OTHER hosts' internals.
    if v4.is_private() {
        return Some("private (RFC1918)");
    }
    if v4.is_link_local() {
        // 169.254.0.0/16, which includes the 169.254.169.254 metadata endpoint.
        return Some("link-local (incl. cloud metadata)");
    }
    if v4.is_broadcast() {
        return Some("broadcast");
    }
    if v4.is_multicast() {
        return Some("multicast");
    }
    if v4.is_unspecified() {
        return Some("unspecified (0.0.0.0)");
    }
    // CGNAT 100.64.0.0/10, routable internally on many networks.
    let o = v4.octets();
    if o[0] == 100 && (o[1] & 0xc0) == 0x40 {
        return Some("carrier-grade NAT (100.64.0.0/10)");
    }
    None
}

fn forbidden_v6(v6: Ipv6Addr) -> Option<&'static str> {
    // ::1 loopback allowed, as for IPv4 above.
    if v6.is_multicast() {
        return Some("multicast");
    }
    if v6.is_unspecified() {
        return Some("unspecified (::)");
    }
    let seg = v6.segments();
    if (seg[0] & 0xfe00) == 0xfc00 {
        return Some("unique-local (fc00::/7)");
    }
    if (seg[0] & 0xffc0) == 0xfe80 {
        return Some("link-local (fe80::/10)");
    }
    None
}

/// Structural + literal-IP validation. Returns the parsed URL, or a reason.
/// Does NOT resolve DNS: a bare hostname passes here and is caught by
/// [`validate_url_resolved`] or [`GuardedResolver`]. Safe to call from a
/// synchronous redirect closure.
pub(crate) fn validate_url(url: &str) -> Result<Url, String> {
    let parsed = Url::parse(url).map_err(|e| format!("invalid URL: {e}"))?;

    match parsed.scheme() {
        "http" | "https" => {}
        "file" => return Err("file:// URLs are not allowed".to_string()),
        other => return Err(format!("unsupported scheme: {other}")),
    }

    match parsed.host() {
        None => return Err("URL has no host".to_string()),
        Some(Host::Domain(host)) => {
            let lower = host.to_ascii_lowercase();
            // localhost is allowed (resolves to loopback), same rationale as the
            // loopback IP ranges. Metadata hosts stay blocked.
            if METADATA_HOSTS.contains(&lower.as_str()) {
                return Err("requests to the cloud metadata host are not allowed".to_string());
            }
        }
        Some(Host::Ipv4(v4)) => {
            if let Some(reason) = forbidden_v4(v4) {
                return Err(format!("requests to {reason} addresses are not allowed"));
            }
        }
        Some(Host::Ipv6(v6)) => {
            if let Some(reason) = forbidden_ip(IpAddr::V6(v6)) {
                return Err(format!("requests to {reason} addresses are not allowed"));
            }
        }
    }

    Ok(parsed)
}

/// The single verdict on a set of resolved addresses.
///
/// Every path that has addresses in hand asks this: the initial fetch, the DNS
/// resolver, and any future caller. One function so the layers cannot drift
/// apart. An empty set is refused too, because "resolved to nothing" is not an
/// approval.
pub(crate) fn guard_resolved_addrs(host: &str, addrs: &[SocketAddr]) -> Result<(), String> {
    let lower = host.to_ascii_lowercase();
    if METADATA_HOSTS.contains(&lower.as_str()) {
        return Err(format!(
            "host {host} names a cloud metadata endpoint, refusing (SSRF guard)"
        ));
    }
    if addrs.is_empty() {
        return Err(format!(
            "host {host} resolved to no addresses, refusing (SSRF guard)"
        ));
    }
    for addr in addrs {
        if let Some(reason) = forbidden_ip(addr.ip()) {
            return Err(format!(
                "host {host} resolves to a {reason} address, refusing (SSRF guard)"
            ));
        }
    }
    Ok(())
}

/// [`validate_url`] plus a DNS resolution pass: a hostname that resolves to any
/// forbidden address is refused.
///
/// This is the pre-flight check for the initial URL, where a clear error beats a
/// connect-time failure. It is NOT the authority on what gets connected to: the
/// addresses this call sees are discarded, and [`GuardedResolver`] resolves
/// again for the real connection and validates that answer. See the module
/// comment for why the resolver is the layer that matters.
pub(crate) async fn validate_url_resolved(url: &str) -> Result<Url, String> {
    let parsed = validate_url(url)?;

    if let Some(Host::Domain(host)) = parsed.host() {
        let host = host.to_string();
        let port = parsed.port_or_known_default().unwrap_or(80);
        let addrs: Vec<SocketAddr> = tokio::net::lookup_host((host.as_str(), port))
            .await
            .map_err(|e| format!("could not resolve host {host}: {e}"))?
            .collect();
        guard_resolved_addrs(&host, &addrs)?;
    }

    Ok(parsed)
}

/// A pending lookup. Boxed because it crosses the [`Lookup`] boundary.
pub(crate) type LookupFuture =
    Pin<Box<dyn Future<Output = Result<Vec<SocketAddr>, String>> + Send>>;

/// How a hostname becomes addresses.
///
/// A trait rather than a closure type, so the guard can be exercised without
/// depending on what the machine's own resolver happens to answer.
pub(crate) trait Lookup: Send + Sync {
    /// Resolve `host`, or explain why it could not be resolved.
    fn lookup(&self, host: &str) -> LookupFuture;
}

/// The production lookup: the system's own DNS.
pub(crate) struct SystemLookup;

impl Lookup for SystemLookup {
    fn lookup(&self, host: &str) -> LookupFuture {
        let host = host.to_string();
        Box::pin(async move {
            tokio::net::lookup_host((host.as_str(), 0))
                .await
                .map(|iter| iter.collect::<Vec<_>>())
                .map_err(|e| format!("could not resolve host {host}: {e}"))
        })
    }
}

/// A reqwest DNS resolver that validates before it hands addresses to the
/// connector, and hands over exactly the addresses it validated.
///
/// reqwest's redirect policy closure is synchronous and cannot resolve a hop's
/// hostname; the resolver can, and it runs for every connection the client
/// opens, redirect hops included. Because the addresses returned here are the
/// ones hyper connects to, the "validated" set and the "connected" set are the
/// same set, which is what makes the rebinding window disappear.
#[derive(Clone)]
pub(crate) struct GuardedResolver {
    lookup: Arc<dyn Lookup>,
}

impl GuardedResolver {
    /// The production resolver: the system's own DNS.
    pub(crate) fn new() -> Self {
        Self::with_lookup(Arc::new(SystemLookup))
    }

    /// A resolver that answers from `lookup` instead of the system.
    pub(crate) fn with_lookup(lookup: Arc<dyn Lookup>) -> Self {
        Self { lookup }
    }

    /// Resolve, then refuse unless [`guard_resolved_addrs`] approves the answer.
    pub(crate) async fn resolve_guarded(&self, host: &str) -> Result<Vec<SocketAddr>, String> {
        let addrs = self.lookup.lookup(host).await?;
        guard_resolved_addrs(host, &addrs)?;
        Ok(addrs)
    }
}

impl Default for GuardedResolver {
    fn default() -> Self {
        Self::new()
    }
}

impl reqwest::dns::Resolve for GuardedResolver {
    fn resolve(&self, name: reqwest::dns::Name) -> reqwest::dns::Resolving {
        let host = name.as_str().to_string();
        let this = self.clone();
        Box::pin(async move {
            let addrs = this
                .resolve_guarded(&host)
                .await
                .map_err(|e| -> Box<dyn std::error::Error + Send + Sync> { e.into() })?;
            let iter: reqwest::dns::Addrs = Box::new(addrs.into_iter());
            Ok(iter)
        })
    }
}

/// Wire the whole guard onto a client builder: the validating DNS resolver plus
/// the per-hop redirect policy.
///
/// Every server-side fetch tool that follows redirects must go through this
/// rather than attaching the two halves by hand, because a client carrying only
/// one half still has a hole: the policy alone cannot see a hop's resolved
/// addresses, and the resolver alone will not stop a hop to a literal
/// `169.254.169.254`. `follow_redirects` false keeps the redirect policy off but
/// still installs the resolver, so the first request is validated either way.
pub(crate) fn guard_client(
    builder: reqwest::ClientBuilder,
    follow_redirects: bool,
) -> reqwest::ClientBuilder {
    let builder = builder.dns_resolver(GuardedResolver::new());
    if follow_redirects {
        builder.redirect(redirect_policy(10))
    } else {
        builder.redirect(reqwest::redirect::Policy::none())
    }
}

/// A reqwest redirect policy that re-checks every hop with [`validate_url`],
/// capped at `max` hops.
///
/// This is the cheap synchronous half. It stops a hop to a literal private
/// address, a metadata hostname, or a non-web scheme before the request is sent.
/// A hop to a hostname that only resolves internally is stopped by
/// [`GuardedResolver`] at connect time, which is the case a synchronous closure
/// structurally cannot look at.
pub(crate) fn redirect_policy(max: usize) -> reqwest::redirect::Policy {
    reqwest::redirect::Policy::custom(move |attempt| {
        if attempt.previous().len() >= max {
            return attempt.error(format!("too many redirects (>{max})"));
        }
        match validate_url(attempt.url().as_str()) {
            Ok(_) => attempt.follow(),
            Err(reason) => attempt.error(format!("redirect blocked by SSRF guard: {reason}")),
        }
    })
}
