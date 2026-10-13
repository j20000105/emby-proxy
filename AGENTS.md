# Emby Rust proxy

Read README.md before changing or deploying this service.

- Preserve streaming backpressure, byte ranges, HEAD, signed query encoding,
  and Emby WebSocket upgrades. Never buffer an entire upstream response.
- Follow HTTP redirects on the proxy. Bound redirect chains, strip credentials
  on cross-origin redirects, and never log tokens, cookies, signed URLs or
  raw reqwest errors.
- Default to https://iris.pvq.cc. An unrecognized or missing Host uses that
  origin; a request URL must never choose an arbitrary upstream.
- Logs rotate by UTC date and retain at most thirty days, including after a
  quiet period or restart.
- Before fleet deployment, pass local protocol and real playback tests and
  complete two separate in-depth code reviews. Record evidence in docs/reviews.md.
- Build release binaries from the committed, pushed revision with Cargo.lock.
  Server changes belong to the separate iris-server deployment repository.
  Every target must clone/pull its deployment revision and execute the tracked
  entry point. Do not replace AxisNow until the staged proxy passes checks.
