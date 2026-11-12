//! Shared network-egress **SSRF gate** for stage- and behavior-driven HTTP.
//!
//! USD asset downloads, `OpenExec` `RestDatasource` polling and the WASM behavior
//! `net` capability reach the network with
//! attacker-influenced inputs — a stage-authored endpoint, or a behavior's
//! hardcoded URL. This crate is the single source of truth for the policy that
//! guards all three consumers.
//!
//! It is a deliberately small **leaf crate** so the low-level behavior host can
//! depend on it without pulling the session layer — the same reason `idtx-auth`
//! was carved out.
//!
//! # Feature `client` (default)
//!
//! The policy **type** ([`NetworkPolicy`]) is always available with no heavy
//! dependency, so a crate can carry a policy (e.g. in its host state) without
//! linking an HTTP stack. The actual validation + locked-down client
//! ([`validate_endpoint`], [`locked_client`], and the address/host predicates)
//! live behind the default-on `client` feature, which pulls `reqwest`. A
//! consumer that only needs to *store* a policy depends with
//! `default-features = false` (e.g. the behavior host on cross-compiled targets
//! that opt out of HTTP); a consumer that actually fetches enables `client`.
//!
//! # Policy (deny-by-default)
//!
//! A request is permitted only when **all** hold:
//! * [`NetworkPolicy`] enables egress and names the host (or the editor explicitly
//!   selects [`NetworkPolicy::public_internet`]);
//! * the scheme is `http` or `https`;
//! * the URL contains no userinfo; host patterns are exact or `*.domain`
//!   wildcards (a bare `*` cannot open egress);
//! * **every** resolved address is public, or is loopback/RFC1918/unique-local
//!   and the host has an explicit `private_hosts` grant. Link-local/metadata,
//!   CGNAT and other special-purpose ranges are always rejected.
//!
//! A permitted request is then issued through a [`locked_client`]: redirects are
//! disabled (a 3xx to an internal host cannot be followed) and DNS is **pinned**
//! to the validated addresses, so a rebind between validation and fetch cannot
//! steer the connection to a blocked address.

/// Default per-response body cap (1 MiB) applied when nothing configures
/// `max_response_mb` (or sets it to `0`). A stored cap of `0` always resolves to
/// this via [`NetworkPolicy::max_response_bytes`] — it is never "unlimited".
pub const DEFAULT_MAX_RESPONSE_BYTES: usize = 1024 * 1024;

/// Asset downloads stream to disk and have a separate, larger default cap.
pub const DEFAULT_MAX_ASSET_MB: u32 = 256;

#[cfg(feature = "client")]
pub mod dns;

/// Network-egress capability shared by USD assets, REST datasources and behavior HTTP.
///
/// Deny-by-default: an unconfigured policy ([`NetworkPolicy::denied`], the
/// [`Default`]) permits **zero** HTTP. To allow a request the policy must both set
/// `allow_network` and list the endpoint's host in `allowed_hosts` — listing is
/// mandatory, so enabling the flag alone still allows nothing. The editor may
/// explicitly select [`NetworkPolicy::public_internet`] for asset browsing.
///
/// Host patterns are matched case-insensitively (trailing dot stripped):
/// * exact — `api.example.com` matches only `api.example.com`.
/// * wildcard — `*.example.com` matches `example.com` **and** any subdomain
///   (`a.example.com`, `a.b.example.com`).
/// * a bare `*` is treated as a literal host and matches nothing, so it cannot be
///   used to silently open egress to every endpoint.
#[derive(Debug, Clone, Default)]
pub struct NetworkPolicy {
    /// Master switch. When `false`, nothing is permitted regardless of
    /// `allowed_hosts`.
    allow_network: bool,
    /// Allowlist of host patterns (exact or `*.domain` wildcards). An endpoint is
    /// rejected unless its host matches one of these.
    allowed_hosts: Vec<String>,
    /// Explicit editor posture: any public destination may be opened. This is
    /// never enabled by a manifest or by a bare `*` host pattern.
    allow_public_hosts: bool,
    /// Hosts permitted to resolve to loopback, RFC1918 or unique-local addresses.
    /// Does not bypass the master switch, normal host gate or metadata blocks.
    private_hosts: Vec<String>,
    /// Per-response body size cap in bytes. `0` means "unset" and resolves to
    /// [`DEFAULT_MAX_RESPONSE_BYTES`] via [`max_response_bytes`](Self::max_response_bytes);
    /// it is **never** treated as unlimited.
    max_response_bytes: usize,
}

impl NetworkPolicy {
    /// The deny-everything policy (no network access). Same as [`Default`].
    #[must_use]
    pub fn denied() -> Self {
        Self::default()
    }

    /// Build a policy from a master switch and a host allowlist. The response body
    /// cap stays at the default (1 MiB); set a different one with
    /// [`with_max_response_mb`](Self::with_max_response_mb).
    #[must_use]
    pub fn new(allow_network: bool, allowed_hosts: Vec<String>) -> Self {
        Self {
            allow_network,
            allowed_hosts,
            ..Self::default()
        }
    }

    /// Explicit editor asset-browsing policy. Unlike the default it permits
    /// arbitrary public hosts; private destinations still need a named grant.
    #[must_use]
    pub fn public_internet() -> Self {
        Self { allow_network: true, allow_public_hosts: true, ..Self::default() }
    }

    /// Permit loopback/RFC1918/unique-local addresses for these host patterns.
    /// Hosts must also pass the normal reachability gate. Never permits metadata,
    /// link-local, CGNAT, multicast or reserved ranges, even when named here.
    #[must_use]
    pub fn with_private_hosts(mut self, hosts: Vec<String>) -> Self {
        self.private_hosts = hosts;
        self
    }

    /// Set the per-response body cap from a megabyte value. `0` keeps the default
    /// (1 MiB) — it never disables the cap. Saturates rather than overflowing on
    /// absurd values.
    #[must_use]
    pub fn with_max_response_mb(mut self, mb: u32) -> Self {
        self.max_response_bytes = (mb as usize).saturating_mul(1024 * 1024);
        self
    }

    /// `true` when nothing can ever be fetched under this policy — either the
    /// switch is off or the allowlist is empty (listing is mandatory).
    #[must_use]
    pub fn is_denied(&self) -> bool {
        !self.allow_network || (!self.allow_public_hosts && self.allowed_hosts.is_empty())
    }

    /// The effective per-response body cap in bytes, resolving an unset (`0`) value
    /// to [`DEFAULT_MAX_RESPONSE_BYTES`].
    #[must_use]
    pub fn max_response_bytes(&self) -> usize {
        if self.max_response_bytes == 0 {
            DEFAULT_MAX_RESPONSE_BYTES
        } else {
            self.max_response_bytes
        }
    }

}

// ---------------------------------------------------------------------------
// Validation + locked-down client (feature `client` — pulls reqwest)
// ---------------------------------------------------------------------------

#[cfg(feature = "client")]
mod client {
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
    use std::time::Duration;

    use super::NetworkPolicy;

    /// A validated endpoint: the full request URL plus the exact socket addresses
    /// it is permitted to connect to. A caller pins its HTTP client to `addrs`
    /// (via [`locked_client`]) so a DNS rebind cannot redirect the connection to
    /// an internal host.
    #[derive(Clone)]
    pub struct ValidatedEndpoint {
        /// Full request URL.
        url: String,
        /// The DNS host to pin, or `None` when the URL already used an IP literal
        /// (nothing to pin — reqwest connects straight to the literal).
        pin_host: Option<String>,
        /// The validated, non-blocked addresses the host resolved to.
        addrs: Vec<SocketAddr>,
    }

    impl ValidatedEndpoint {
        #[must_use]
        pub fn url(&self) -> &str { &self.url }

        #[must_use]
        pub fn addresses(&self) -> &[SocketAddr] { &self.addrs }
    }

    impl std::fmt::Debug for ValidatedEndpoint {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.debug_struct("ValidatedEndpoint")
                .field("url", &redact_url(&self.url))
                .field("pin_host", &self.pin_host)
                .field("addrs", &self.addrs).finish()
        }
    }

    /// DNS validation is bounded independently of the transport. Blocking OS
    /// lookups run in a fixed-size pool; timed-out jobs cannot grow thread count.
    pub const DNS_TIMEOUT: Duration = Duration::from_secs(5);

    /// `true` when `ip` is in a range an untrusted endpoint must never reach:
    /// loopback, link-local (incl. the `169.254.169.254` cloud-metadata address),
    /// private / unique-local, carrier-grade NAT, unspecified, broadcast,
    /// multicast, documentation, IETF protocol-assignment (`192.0.0.0/24`), 6to4
    /// relay anycast (`192.88.99.0/24`), benchmarking (`198.18.0.0/15`) and
    /// reserved / class E (`240.0.0.0/4`) ranges. IPv4-mapped / -compatible /
    /// NAT64-embedded IPv6 addresses are unwrapped and re-checked as IPv4. For
    /// IPv6 the policy is **allow-by-exception** (see [`is_blocked_ipv6`]): only a
    /// routable global-unicast address passes. Fail-closed: anything not clearly a
    /// routable public address is treated as blocked. (`Ipv4Addr::is_global`
    /// would capture all of this in one call, but it is still unstable
    /// (`feature(ip)`), so the ranges are enumerated here against the stable
    /// predicates.)
    #[must_use]
    pub fn is_blocked_ip(ip: IpAddr) -> bool {
        match ip {
            IpAddr::V4(v4) => is_blocked_ipv4(v4),
            IpAddr::V6(v6) => {
                // Unwrap IPv4-in-IPv6 forms so `::ffff:127.0.0.1` cannot sneak past
                // the IPv4 range checks.
                if let Some(mapped) = v6.to_ipv4_mapped() {
                    return is_blocked_ipv4(mapped);
                }
                if let Some(compat) = ipv4_compatible(v6) {
                    return is_blocked_ipv4(compat);
                }
                // NAT64 (`64:ff9b::/96`, RFC 6052): re-check the embedded IPv4, so
                // `64:ff9b::10.0.0.1` is classified as the private host it really
                // reaches through a NAT64 gateway, while `64:ff9b::8.8.8.8` resolves
                // to the public host.
                if let Some(nat64) = ipv4_from_nat64(v6) {
                    return is_blocked_ipv4(nat64);
                }
                is_blocked_ipv6(v6)
            }
        }
    }

    fn is_private_destination(ip: IpAddr) -> bool {
        match ip {
            IpAddr::V4(v4) => v4.is_loopback() || v4.is_private(),
            IpAddr::V6(v6) => {
                if let Some(v4) = v6.to_ipv4_mapped().or_else(|| ipv4_compatible(v6)).or_else(|| ipv4_from_nat64(v6)) {
                    return is_private_destination(IpAddr::V4(v4));
                }
                v6.is_loopback() || v6.is_unique_local()
            }
        }
    }

    fn is_blocked_ipv4(v4: Ipv4Addr) -> bool {
        let o = v4.octets();
        v4.is_loopback()
            || v4.is_private()
            || v4.is_link_local() // covers 169.254.0.0/16, incl. 169.254.169.254
            || v4.is_unspecified()
            || v4.is_broadcast()
            || v4.is_multicast()
            || v4.is_documentation() // 192.0.2/24, 198.51.100/24, 203.0.113/24
            || o[0] == 0 // 0.0.0.0/8 "this network"
            || is_cgnat(v4) // 100.64.0.0/10 carrier-grade NAT / shared address space
            || (o[0] == 192 && o[1] == 0 && o[2] == 0) // 192.0.0.0/24 IETF protocol assignments
            || (o[0] == 192 && o[1] == 88 && o[2] == 99) // 192.88.99.0/24 6to4 relay anycast
            || (o[0] == 198 && (o[1] == 18 || o[1] == 19)) // 198.18.0.0/15 benchmarking
            || o[0] >= 240 // 240.0.0.0/4 reserved (class E), incl. 255.255.255.255
    }

    /// 100.64.0.0/10 — RFC 6598 shared address space (not covered by `is_private`).
    fn is_cgnat(v4: Ipv4Addr) -> bool {
        let o = v4.octets();
        o[0] == 100 && (64..=127).contains(&o[1])
    }

    fn is_blocked_ipv6(v6: Ipv6Addr) -> bool {
        let s = v6.segments();
        let seg0 = s[0];
        // **Allow-by-exception, not a denylist.** An IPv6 address is permitted only
        // when it is a routable global-unicast address (`2000::/3`). Everything else
        // fails closed: loopback `::1`, unspecified `::`, unique-local `fc00::/7`,
        // link-local `fe80::/10`, deprecated site-local `fec0::/10`, multicast
        // `ff00::/8`, discard-only `100::/64`, and any other unenumerated non-global
        // range are all blocked by this single gate — so a range we never thought to
        // list cannot slip through (the previous denylist let `fec0::/10` and others
        // pass). NAT64 `64:ff9b::/96` is handled earlier in `is_blocked_ip` by
        // unwrapping its embedded IPv4.
        if (seg0 & 0xe000) != 0x2000 {
            return true;
        }
        // Special-purpose ranges that DO live inside `2000::/3` but are not routable
        // public hosts (IANA IPv6 Special-Purpose Address Registry / RFC 6890):
        (seg0 == 0x2001 && ((s[1] & 0xfe00) == 0 || s[1] == 0x0db8)) // 2001::/23 IETF protocol (Teredo/benchmarking/ORCHIDv2) + 2001:db8::/32 documentation
            || seg0 == 0x2002                       // 2002::/16 6to4 (embeds a possibly-internal IPv4)
            || (seg0 == 0x3fff && (s[1] & 0xf000) == 0) // 3fff::/20 documentation (RFC 9637)
    }

    /// The IPv4 address embedded in a well-known NAT64 (`64:ff9b::/96`, RFC 6052)
    /// IPv6 address — the low 32 bits. The `64:ff9b:1::/48` local-use prefix and
    /// other lengths are not unwrapped here; they are not global-unicast and are
    /// blocked by the `2000::/3` gate in [`is_blocked_ipv6`] regardless.
    fn ipv4_from_nat64(v6: Ipv6Addr) -> Option<Ipv4Addr> {
        let s = v6.segments();
        if s[0] == 0x0064 && s[1] == 0xff9b && s[2..6].iter().all(|&x| x == 0) {
            let hi = s[6].to_be_bytes();
            let lo = s[7].to_be_bytes();
            Some(Ipv4Addr::new(hi[0], hi[1], lo[0], lo[1]))
        } else {
            None
        }
    }

    /// The IPv4 address embedded in an `::a.b.c.d` IPv4-compatible IPv6 address,
    /// if any. (`to_ipv4_mapped` handles the `::ffff:a.b.c.d` form; this handles
    /// the deprecated-but-still-parseable compatible form.) `::` / `::1` are
    /// excluded so the unspecified/loopback checks keep handling them.
    fn ipv4_compatible(v6: Ipv6Addr) -> Option<Ipv4Addr> {
        let s = v6.segments();
        if s[..6].iter().all(|&x| x == 0) && (s[6] != 0 || s[7] > 1) {
            let hi = s[6].to_be_bytes();
            let lo = s[7].to_be_bytes();
            Some(Ipv4Addr::new(hi[0], hi[1], lo[0], lo[1]))
        } else {
            None
        }
    }

    /// A loggable, secret-free rendering of `url`: scheme, host, port and path
    /// only. Userinfo (`user:pass@`), the query string and the fragment are
    /// dropped — a stage-authored or behavior endpoint commonly carries an API key
    /// or Basic credentials there, and every `url` logging site in the REST and
    /// behavior-`net` paths routes through this so a secret never reaches the logs
    /// / conductor preview output. Returns a fixed placeholder (never the raw
    /// string, which may itself be the secret) when the URL cannot be parsed.
    #[must_use]
    pub fn redact_url(url: &str) -> String {
        match reqwest::Url::parse(url) {
            Ok(u) if matches!(u.scheme(), "http" | "https") && u.host_str().is_some() => {
                let scheme = u.scheme();
                let host = u.host_str().unwrap_or("");
                match u.port() {
                    Some(p) => format!("{scheme}://{host}:{p}{}", u.path()),
                    None => format!("{scheme}://{host}{}", u.path()),
                }
            }
            _ => "<unparseable-url>".to_string(),
        }
    }

    /// Case-insensitive match of `host` against one allowlist `pattern`. See
    /// [`NetworkPolicy`] for the pattern grammar.
    #[must_use]
    pub fn host_matches_pattern(host: &str, pattern: &str) -> bool {
        let normalize = |value: &str| {
            let value = value.trim().trim_end_matches('.').trim_start_matches('[').trim_end_matches(']');
            value.parse::<IpAddr>().map_or_else(|_| value.to_ascii_lowercase(), |ip| ip.to_string())
        };
        let host = normalize(host);
        let pattern = normalize(pattern);
        if let Some(suffix) = pattern.strip_prefix("*.") {
            // `*.example.com` matches `example.com` and any subdomain of it. A bare
            // `*` leaves an empty suffix, which matches nothing here.
            !suffix.is_empty() && (host == suffix || host.ends_with(&format!(".{suffix}")))
        } else {
            !pattern.is_empty() && host == pattern
        }
    }

    /// `true` when `host` matches any allowlist pattern.
    #[must_use]
    pub fn host_matches_allowlist(host: &str, patterns: &[String]) -> bool {
        patterns.iter().any(|p| host_matches_pattern(host, p))
    }

    /// Why [`validate_endpoint`] rejected a request. The two variants carry the
    /// same human-readable reason string the error always did (and
    /// [`Display`](std::fmt::Display) renders exactly that reason), but splitting
    /// them lets a caller log a **policy refusal** differently from an endpoint
    /// that policy *permitted* but that could not be reached — so a DNS outage is
    /// never reported as an allowlist denial.
    #[derive(Debug, Clone)]
    pub enum EndpointRejection {
        /// The request was refused by policy **before any network attempt**:
        /// egress disabled / empty allowlist, a non-`http`/`https` scheme, a host
        /// not on the allowlist, a malformed URL, or a host that resolves to a
        /// blocked (private / internal) address. This does not change until the
        /// policy config or the URL changes — it is not a transient failure.
        Policy(String),
        /// The endpoint passed every policy check but could not be resolved to a
        /// usable public address (DNS returned an error or no addresses). Policy
        /// permits it; the host is simply unreachable right now — commonly a DNS
        /// or connectivity problem on the device, **not** an allowlist issue.
        Unresolved(String),
    }

    impl EndpointRejection {
        /// The human-readable reason (the same text the error always carried).
        #[must_use]
        pub fn reason(&self) -> &str {
            match self {
                Self::Policy(r) | Self::Unresolved(r) => r,
            }
        }

        /// `true` when policy refused the endpoint; `false` when policy permitted
        /// it but it could not be resolved / reached.
        #[must_use]
        pub fn is_policy(&self) -> bool {
            matches!(self, Self::Policy(_))
        }
    }

    impl std::fmt::Display for EndpointRejection {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str(self.reason())
        }
    }

    /// Validate a complete request URL against `policy` and resolve it to a
    /// permitted set of socket addresses. Returns [`EndpointRejection`] (the
    /// caller logs it; no request is made) when the policy denies network access,
    /// the URL is malformed, the scheme is not `http`/`https`, the host is not
    /// allowlisted, or the host resolves to a blocked / empty address set. The
    /// rejection is classified as [`EndpointRejection::Policy`] or
    /// [`EndpointRejection::Unresolved`] so the caller can log a policy refusal
    /// distinctly from an unreachable host.
    ///
    /// This runs once per request (datasource build, or a behavior `net` call) —
    /// not a per-frame hot path — so it is not instrumented behind a metrics flag.
    ///
    /// # Errors
    ///
    /// Returns a classified [`EndpointRejection`] carrying a human-readable reason
    /// (for logging).
    pub fn validate_endpoint(
        url: &str,
        policy: &NetworkPolicy,
    ) -> Result<ValidatedEndpoint, EndpointRejection> {
        validate_endpoint_with_timeout(url, policy, DNS_TIMEOUT)
    }

    /// Like [`validate_endpoint`], with a caller's remaining DNS time budget.
    /// The shared five-second DNS ceiling still applies.
    pub fn validate_endpoint_with_timeout(
        url: &str,
        policy: &NetworkPolicy,
        timeout: Duration,
    ) -> Result<ValidatedEndpoint, EndpointRejection> {
        validate_with_resolver(url, policy, timeout.min(DNS_TIMEOUT), super::dns::resolve)
    }

    fn validate_with_resolver(
        url: &str,
        policy: &NetworkPolicy,
        timeout: Duration,
        resolve: impl FnOnce(&str, u16, Duration) -> std::io::Result<Vec<SocketAddr>>,
    ) -> Result<ValidatedEndpoint, EndpointRejection> {
        use EndpointRejection::{Policy, Unresolved};

        if policy.is_denied() {
            return Err(Policy(
                "network egress is disabled (no [network] allowlist configured)".into(),
            ));
        }

        let mut url = reqwest::Url::parse(url).map_err(|e| Policy(format!("invalid URL: {e}")))?;

        match url.scheme() {
            "http" | "https" => {}
            other => {
                return Err(Policy(format!(
                    "scheme {other:?} not allowed (only http/https)"
                )))
            }
        }
        if !url.username().is_empty() || url.password().is_some() {
            return Err(Policy("URL userinfo is not permitted; use origin-scoped credentials".into()));
        }
        url.set_fragment(None);

        let host = url
            .host_str()
            .ok_or_else(|| Policy("URL has no host".to_string()))?
            // `host_str` returns IPv6 literals bracketed (`[::1]`); strip for
            // matching and for the IP-literal test below.
            .trim_start_matches('[')
            .trim_end_matches(']')
            .to_string();

        if !policy.allow_public_hosts && !host_matches_allowlist(&host, &policy.allowed_hosts) {
            return Err(Policy(format!(
                "host {host:?} is not in the network allowlist"
            )));
        }

        // Resolve + validate every address the host maps to. Fail-closed:
        // if ANY resolved address is blocked, reject the whole endpoint (a mixed
        // public/private result is a classic rebinding setup). A DNS failure here
        // is `Unresolved` (the host passed the allowlist); a resolved-but-blocked
        // address is a `Policy` rejection (the SSRF gate refused it).
        let port = url.port_or_known_default().ok_or_else(|| Policy("URL has no port".into()))?;
        let literal = host.parse::<IpAddr>().ok();
        let addrs = match literal {
            Some(ip) => vec![SocketAddr::new(ip, port)],
            None => resolve(&host, port, timeout)
                .map_err(|e| Unresolved(format!("could not resolve host {host:?}: {e}")))?,
        };
        if addrs.is_empty() {
            return Err(Unresolved(format!("host {host:?} resolved to no addresses")));
        }
        let private_grant = host_matches_allowlist(&host, &policy.private_hosts);
        if let Some(bad) = addrs.iter().find(|a| is_blocked_ip(a.ip())
            && !(private_grant && is_private_destination(a.ip()))) {
            return Err(Policy(format!(
                "host {host:?} resolves to a blocked address {}",
                bad.ip()
            )));
        }

        // Pin DNS only when the URL used a hostname; an IP literal needs no pinning
        // (reqwest connects straight to it, and it was already range-checked above).
        let pin_host = literal.is_none().then_some(host);

        Ok(ValidatedEndpoint {
            url: url.to_string(),
            pin_host,
            addrs,
        })
    }

    /// Build a blocking HTTP client locked down to a [`ValidatedEndpoint`]:
    /// redirects are **disabled** and DNS for the endpoint host is **pinned** to
    /// the validated addresses, so a 3xx or a DNS rebind cannot steer the
    /// connection to a blocked host between validation and fetch. `timeout` bounds
    /// blocking operations. Also set `RequestBuilder::timeout` on each request:
    /// that sets reqwest's underlying total deadline, including a streamed body.
    ///
    /// # Errors
    ///
    /// Returns the `reqwest::Error` if the TLS/connection backend fails to
    /// initialise (effectively never for the vendored rustls backend).
    pub fn locked_client(
        validated: &ValidatedEndpoint,
        timeout: Duration,
    ) -> reqwest::Result<reqwest::blocking::Client> {
        let mut builder = reqwest::blocking::Client::builder()
            .connect_timeout(timeout.min(Duration::from_secs(30)))
            .timeout(timeout)
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy();
        if let Some(host) = &validated.pin_host {
            builder = builder.resolve_to_addrs(host, &validated.addrs);
        }
        builder.build()
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        fn ip(s: &str) -> IpAddr {
            s.parse().unwrap()
        }

        #[test]
        fn blocks_private_and_internal_ipv4_ranges() {
            for addr in [
                "127.0.0.1",       // loopback
                "127.255.255.254", // loopback /8
                "10.0.0.1",        // private
                "172.16.5.4",      // private
                "192.168.1.1",     // private
                "169.254.0.1",     // link-local
                "169.254.169.254", // cloud metadata
                "100.64.0.1",      // CGNAT
                "0.0.0.0",         // unspecified / this-network
                "0.1.2.3",         // 0.0.0.0/8
                "255.255.255.255", // broadcast
                "224.0.0.1",       // multicast
                "192.0.0.1",       // IETF protocol assignments 192.0.0.0/24
                "192.0.2.1",       // documentation TEST-NET-1
                "198.18.0.1",      // benchmarking 198.18.0.0/15
                "240.0.0.1",       // reserved / class E 240.0.0.0/4
            ] {
                assert!(is_blocked_ip(ip(addr)), "{addr} should be blocked");
            }
        }

        #[test]
        fn allows_public_ipv4() {
            for addr in ["1.1.1.1", "8.8.8.8", "93.184.216.34"] {
                assert!(!is_blocked_ip(ip(addr)), "{addr} should be allowed");
            }
        }

        #[test]
        fn blocks_internal_ipv6_and_mapped_v4() {
            for addr in [
                "::1",                    // loopback
                "::",                     // unspecified
                "fc00::1",                // unique local
                "fd12:3456::1",           // unique local
                "fe80::1",                // link local
                "fec0::1",                // deprecated site-local (denylist regression)
                "ff02::1",                // multicast
                "100::1",                 // discard-only 100::/64
                "2001::1",                // Teredo / IETF protocol 2001::/23
                "2001:2::1",              // benchmarking 2001:2::/48 (within 2001::/23)
                "2002:7f00:1::1",         // 6to4 embedding 127.0.0.1
                "3fff::1",                // documentation 3fff::/20 (RFC 9637)
                "::ffff:127.0.0.1",       // IPv4-mapped loopback
                "::ffff:169.254.169.254", // IPv4-mapped metadata
                "64:ff9b::a00:1",         // NAT64 of 10.0.0.1 (private embedded)
                "64:ff9b::7f00:1",        // NAT64 of 127.0.0.1 (loopback embedded)
                "2001:db8::1",            // documentation 2001:db8::/32
            ] {
                assert!(is_blocked_ip(ip(addr)), "{addr} should be blocked");
            }
            // Public IPv6 addresses are allowed, including a NAT64-wrapped public
            // IPv4 (the embedded host is what is actually reached).
            assert!(!is_blocked_ip(ip("2606:4700:4700::1111")));
            assert!(!is_blocked_ip(ip("64:ff9b::808:808"))); // NAT64 of 8.8.8.8
        }

        #[test]
        fn host_pattern_exact_and_wildcard() {
            assert!(host_matches_pattern("api.example.com", "api.example.com"));
            assert!(host_matches_pattern("API.Example.COM", "api.example.com")); // case-insensitive
            assert!(host_matches_pattern("api.example.com.", "api.example.com")); // trailing dot
            assert!(!host_matches_pattern("evil.com", "api.example.com"));

            // `*.example.com` matches the apex and any subdomain depth.
            assert!(host_matches_pattern("example.com", "*.example.com"));
            assert!(host_matches_pattern("a.example.com", "*.example.com"));
            assert!(host_matches_pattern("a.b.example.com", "*.example.com"));
            assert!(!host_matches_pattern("example.com.evil.com", "*.example.com"));
            assert!(!host_matches_pattern("notexample.com", "*.example.com"));

            // A bare `*` matches nothing (no silent allow-all).
            assert!(!host_matches_pattern("anything.com", "*"));
        }

        #[test]
        fn denied_policy_rejects_everything() {
            let denied = NetworkPolicy::denied();
            assert!(denied.is_denied());
            assert!(validate_endpoint("https://api.example.com/x", &denied).is_err());

            // allow_network on but no hosts listed is still denied (explicit list
            // is mandatory).
            let empty = NetworkPolicy::new(true, vec![]);
            assert!(empty.is_denied());
            assert!(validate_endpoint("https://api.example.com/x", &empty).is_err());
        }

        #[test]
        fn rejects_non_http_schemes() {
            let policy = NetworkPolicy::new(true, vec!["example.com".into()]);
            for url in [
                "file:///etc/passwd",
                "ftp://example.com/x",
                "gopher://example.com",
            ] {
                assert!(
                    validate_endpoint(url, &policy).is_err(),
                    "{url} should be rejected"
                );
            }
        }

        #[test]
        fn rejects_host_not_in_allowlist() {
            let policy = NetworkPolicy::new(true, vec!["api.example.com".into()]);
            // Public but unlisted host (1.1.1.1 literal) is rejected before any DNS.
            assert!(validate_endpoint("https://1.1.1.1/x", &policy).is_err());
        }

        #[test]
        fn rejects_url_userinfo_instead_of_implicitly_sending_basic_auth() {
            let policy = NetworkPolicy::new(true, vec!["1.1.1.1".into()]);
            for url in ["http://user:secret@1.1.1.1/data", "https://user@1.1.1.1/data"] {
                assert!(validate_endpoint(url, &policy).is_err(), "URL userinfo must be rejected");
            }
        }

        #[test]
        fn private_access_is_explicit_host_scoped_and_never_opens_metadata() {
            for host in ["127.0.0.1", "10.0.0.1", "172.16.0.1", "192.168.1.2", "::1", "fd00::1", "::ffff:127.0.0.1"] {
                let authority = if host.contains(':') { format!("[{host}]") } else { host.to_owned() };
                let url = format!("http://{authority}/asset");
                let listed = NetworkPolicy::new(true, vec![host.into()]);
                assert!(validate_endpoint(&url, &listed).is_err());
                assert!(validate_endpoint(&url, &listed.clone().with_private_hosts(vec!["other.test".into()])).is_err());
                assert!(validate_endpoint(&url, &listed.with_private_hosts(vec![host.into()])).is_ok(), "{host}");
            }
            for host in ["169.254.169.254", "169.254.170.2", "100.100.100.200", "0.0.0.0", "224.0.0.1", "fe80::1", "::ffff:169.254.169.254", "64:ff9b::a9fe:a9fe"] {
                let authority = if host.contains(':') { format!("[{host}]") } else { host.to_owned() };
                let policy = NetworkPolicy::new(true, vec![host.into()]).with_private_hosts(vec![host.into()]);
                assert!(validate_endpoint(&format!("http://{authority}/"), &policy).is_err(), "{host}");
            }
            let disabled = NetworkPolicy::new(false, vec!["127.0.0.1".into()]).with_private_hosts(vec!["127.0.0.1".into()]);
            assert!(validate_endpoint("http://127.0.0.1", &disabled).is_err());
            assert!(validate_endpoint("http://127.0.0.1", &NetworkPolicy::public_internet()).is_err());
            assert!(validate_endpoint("http://1.1.1.1", &NetworkPolicy::public_internet()).is_ok());
        }

        #[test]
        fn mixed_dns_answers_are_rejected_and_accepted_answers_are_pinned() {
            let policy = NetworkPolicy::new(true, vec!["never-resolves.invalid".into()]);
            let result = validate_with_resolver("http://never-resolves.invalid/", &policy, DNS_TIMEOUT, |_, _, _| {
                Ok(vec!["1.1.1.1:80".parse().unwrap(), "127.0.0.1:80".parse().unwrap()])
            });
            assert!(result.unwrap_err().is_policy());

            let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
            let address = server.server_addr().to_ip().unwrap();
            let url = format!("http://never-resolves.invalid:{}/x?token=secret#fragment", address.port());
            let permitted = policy.with_private_hosts(vec!["never-resolves.invalid".into()]);
            let validated = validate_with_resolver(&url, &permitted, DNS_TIMEOUT, |host, port, _| {
                assert_eq!(host, "never-resolves.invalid");
                assert_eq!(port, address.port());
                Ok(vec![address])
            }).unwrap();
            assert!(!format!("{validated:?}").contains("secret"));
            assert!(!validated.url().contains('#'));
            let worker = std::thread::spawn(move || {
                let request = server.recv_timeout(Duration::from_secs(2)).unwrap().expect("pinned request");
                let host = request.headers().iter().find(|h| h.field.equiv("Host")).unwrap().value.as_str().to_owned();
                request.respond(tiny_http::Response::from_string("pinned")).unwrap();
                host
            });
            let client = locked_client(&validated, Duration::from_secs(1)).unwrap();
            let response = client.get(validated.url()).timeout(Duration::from_secs(1)).send().unwrap();
            assert_eq!(response.remote_addr(), Some(address));
            assert_eq!(response.text().unwrap(), "pinned");
            assert_eq!(worker.join().unwrap(), format!("never-resolves.invalid:{}", address.port()));
        }

        #[test]
        fn rejects_allowlisted_host_that_resolves_to_loopback() {
            // `localhost` is a hostname that resolves to a blocked address; even if
            // a (foolish) allowlist names it, the address-range check rejects it.
            let policy = NetworkPolicy::new(true, vec!["localhost".into()]);
            assert!(validate_endpoint("http://localhost/x", &policy).is_err());
        }

        #[test]
        fn rejects_private_ip_literal_even_when_allowlisted() {
            let policy = NetworkPolicy::new(true, vec!["169.254.169.254".into()]);
            assert!(validate_endpoint("http://169.254.169.254/latest/meta-data", &policy).is_err());
        }

        #[test]
        fn classifies_policy_refusal_vs_unresolved_host() {
            // Egress disabled → policy refusal.
            assert!(validate_endpoint("https://api.example.com/x", &NetworkPolicy::denied())
                .unwrap_err()
                .is_policy());

            // Host not on the allowlist → policy refusal (no DNS attempted).
            let listed = NetworkPolicy::new(true, vec!["api.example.com".into()]);
            let err = validate_endpoint("https://evil.com/x", &listed).unwrap_err();
            assert!(err.is_policy(), "unlisted host should be a policy rejection: {err}");

            // A resolved-but-blocked (loopback) address → still a policy refusal.
            let loop_listed = NetworkPolicy::new(true, vec!["localhost".into()]);
            assert!(validate_endpoint("http://localhost/x", &loop_listed)
                .unwrap_err()
                .is_policy());

            // An allowlisted host that cannot resolve is NOT a policy rejection:
            // the `.invalid` TLD (RFC 2606/6761) is guaranteed never to resolve,
            // so this is the DNS-failure case that must no longer be reported as
            // an allowlist denial.
            let any_invalid = NetworkPolicy::new(true, vec!["*.invalid".into()]);
            let err = validate_endpoint("https://nope.invalid/x", &any_invalid).unwrap_err();
            assert!(
                !err.is_policy(),
                "unresolvable host should classify as Unresolved, not Policy: {err}"
            );
        }

        #[test]
        fn redact_url_strips_userinfo_and_query() {
            // Userinfo credentials and query secrets are dropped; scheme/host/path kept.
            assert_eq!(
                redact_url("https://user:s3cret@api.example.com/v1/data?api_key=abc123"),
                "https://api.example.com/v1/data"
            );
            // Port is preserved (it is not a secret and aids debugging).
            assert_eq!(
                redact_url("http://host.example.com:8443/path?token=zzz"),
                "http://host.example.com:8443/path"
            );
            // Unparseable input never echoes the raw (possibly-secret) string.
            assert_eq!(redact_url("::not a url::"), "<unparseable-url>");
        }
    }
}

#[cfg(feature = "client")]
pub use client::{
    host_matches_allowlist, host_matches_pattern, is_blocked_ip, locked_client, redact_url,
    validate_endpoint, validate_endpoint_with_timeout, EndpointRejection, ValidatedEndpoint, DNS_TIMEOUT,
};

#[cfg(test)]
mod policy_tests {
    use super::*;

    #[test]
    fn max_response_bytes_defaults_and_resolves() {
        // Unconfigured → default 1 MiB (never "unlimited").
        assert_eq!(
            NetworkPolicy::new(true, vec![]).max_response_bytes(),
            DEFAULT_MAX_RESPONSE_BYTES
        );
        // `with_max_response_mb(0)` keeps the default.
        assert_eq!(
            NetworkPolicy::denied()
                .with_max_response_mb(0)
                .max_response_bytes(),
            DEFAULT_MAX_RESPONSE_BYTES
        );
        // A configured MB value resolves to that many MiB.
        assert_eq!(
            NetworkPolicy::new(true, vec!["x".into()])
                .with_max_response_mb(5)
                .max_response_bytes(),
            5 * 1024 * 1024
        );
        // Absurd values saturate instead of overflowing.
        assert_eq!(
            NetworkPolicy::new(true, vec!["x".into()])
                .with_max_response_mb(u32::MAX)
                .max_response_bytes(),
            (u32::MAX as usize).saturating_mul(1024 * 1024)
        );
    }
}
