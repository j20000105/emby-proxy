# Deployment review record

Date: 2026-10-13. Two separate review passes are required before fleet rollout.

## Pass 1 — protocol and security

Reviewed routing and request-target parsing, every redirect transition,
credential propagation, DNS resolution, framing, streaming lifecycle,
WebSocket upgrade handling and log contents.

Findings fixed:

- Require origin-form paths beginning with `/`. Concatenating another URI
  form could accidentally change the configured hostname. Absolute-form,
  CONNECT and asterisk-form requests fail closed; `//` remains on the origin.
- Reject private/special-use IP literals and DNS results in the connector,
  including alternate numeric IPv4 and mapped IPv6 forms. No second DNS
  lookup occurs between checking an address and connecting to it.
- Require both Connection/Upgrade handshake headers before opening a
  WebSocket tunnel, and validate the upstream upgrade response.
- Move the complete access-log guard into the stream/tunnel. Account for
  HTTP bodies whose declared length completes before an EOF poll.

Evidence: rustfmt, Clippy with warnings denied, three network-policy unit
tests and the end-to-end protocol suite pass. The suite covers default and
explicit hosts, missing Host, redirect limits and loops, signed query bytes,
credential isolation, POST replay, Range/416, HEAD, compressed byte fidelity,
hop headers, first-byte streaming, WebSocket data and log redaction/retention.

Real playback on the development host followed the Iris origin's 302 to its
media origin. Start, 1 MiB seek and suffix requests each returned identical
65,536-byte content to a direct authenticated-origin request followed by a
credential-free media request. The client received 206 without Location;
HEAD returned 200 with no body. Fixture credentials remain outside Git.

## Pass 2 — lifecycle, release and deployment

Reviewed request buffering and cancellation, response-body ownership,
timeout scope, UTC retention, shutdown, static linkage, CI privileges,
artifact identity, systemd sandbox permissions, cutover and rollback.

Findings fixed:

- Per-request body limits alone did not bound concurrent buffering. Added a
  shared 64 MiB reservation budget; large concurrent uploads receive 503 while
  empty API and video GET requests continue. A socket-level regression test
  fills the budget with an unfinished upload and checks both behaviors.
- Restaging a release must not truncate an executable already in use.
  Deployment extracts into a temporary directory and compares or renames it.
- AxisNow retirement must preserve exact packages and stopped runtime state.
  The deployment contract saves root-only recovery archives before purge,
  validates checksums and automatically restores a failed activation. Recovery
  unpacks packages, restores runtime state, then configures the packages.
- Normal proxy upgrades must not recreate retired AxisNow service masks.
  Service handling is conditional on the audited packages being installed.

Verification: native format/lint/unit/protocol checks pass, including the new
buffer-budget regression. Deployment Bash scripts pass ShellCheck and syntax
checks; systemd directives validate with local executable placeholders. The
first GitHub Actions run built and tested the static musl binary successfully.
The final tagged build must pass those same gates before its checksum is
pinned in the deployment repository. Rollback was reviewed but is not claimed
to have been rehearsed against a production AxisNow installation.
