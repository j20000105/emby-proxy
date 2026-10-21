# Emby Rust proxy

Read README.md before changing or deploying this service.

- Preserve streaming backpressure, byte ranges, HEAD, signed query encoding,
  and Emby WebSocket upgrades. Never buffer an entire upstream response.
- When redirect_follow_domains is configured, follow HTTP redirects only to
  those domains (DNS label boundaries, including subdomains); pass others through.
  Preserve the legacy behavior when the follow list is omitted.
  Preserve the status and signed Location when passing a redirect to clients.
  Bound redirect chains, strip credentials
  on cross-origin redirects, and never log tokens, cookies, signed URLs or
  raw reqwest errors.
- Require an explicitly configured default_upstream; never supply a built-in
  origin. Missing configuration must fail startup with a clear stderr message.
  An unrecognized or missing Host uses the configured origin; a request URL
  must never choose an arbitrary upstream.
- Keep real server domains, addresses, credentials and deployment details out
  of public files. Use reserved example domains in documentation and fixtures.
- Logs rotate by UTC date and retain at most thirty days, including after a
  quiet period or restart.
- Before fleet deployment, pass local protocol and real playback tests and
  complete two separate in-depth code reviews. Record evidence in docs/reviews.md.
- Build release binaries from the committed, pushed revision with Cargo.lock.
  Server changes belong to the separate private deployment repository.
  Every target must clone/pull its deployment revision and execute the tracked
  entry point. Do not replace the existing proxy until the staged proxy passes checks.
