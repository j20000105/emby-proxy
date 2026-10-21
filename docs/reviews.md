# Deployment review record

Historical evidence below has been redacted to omit private infrastructure
identifiers. Dates and test outcomes refer to the original review sessions.

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

Real playback on the development host followed the configured origin's 302 to its
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
- Retirement of the previous proxy must preserve exact packages and stopped runtime state.
  The deployment contract saves root-only recovery archives before purge,
  validates checksums and automatically restores a failed activation. Recovery
  unpacks packages, restores runtime state, then configures the packages.
- Normal proxy upgrades must not recreate service masks for the retired proxy.
  Service handling is conditional on the audited packages being installed.

Verification: native format/lint/unit/protocol checks pass, including the new
buffer-budget regression. Deployment Bash scripts pass ShellCheck and syntax
checks; systemd directives validate with local executable placeholders. The
first GitHub Actions run built and tested the static musl binary successfully.
The final tagged build must pass those same gates before its checksum is
pinned in the deployment repository. Rollback was reviewed but is not claimed
to have been rehearsed against a production installation of the previous proxy.

## One-command installer follow-up

The separately requested `install.sh` was reviewed for URL/config quoting,
terminal input, root-only paths, release identity, checksum verification,
immutable release directories, concurrent installer exclusion and failure
cleanup. Installation prompts for the default upstream; update preserves the
existing configuration and logs. Configuration validation precedes switching
the executable. An update that cannot start restores the prior release.

`tests/installer.sh` exercises the actual installer functions in temporary
directories, replacing only network downloads and systemd with fixtures. It
verifies fresh installation, preserved customized configuration/logs, a new
release, failed-start rollback and checksum rejection. ShellCheck and Bash
syntax checks pass. Production uses the separate tracked fleet entry point;
this fixture test is not a claim of an additional production reinstall.

## v0.2.0, pass 1 — file operations and reload boundary

Performed as a separate self-review of the completed config-management change;
no independent human review is claimed. Reviewed the command grammar, exact
host normalization, TOML validation, file-lock lifetime, atomic replacement,
metadata preservation, Unix socket permissions, peer credentials, request/reply
limits, and the full validate/build/swap sequence.

Findings addressed:

- Reject malformed hosts/ports, conflicting normalized route keys, invalid
  resource bounds, invalid socket filenames and unknown configuration fields.
  Bound configuration reads to 1 MiB and reject non-regular files before opening
  them, so a named pipe cannot hang a reload request.
- Serialize CLI writers with a separate lock that survives atomic renames;
  preserve ownership, mode, comments and inline value comments. Relative config
  filenames must fsync the current directory after replacement.
- Validate before contacting the daemon, then re-read and validate inside the
  daemon. A request for a different config file cannot reload this instance.
  Build the new HTTP client before swapping one shared configuration snapshot.
- Reject the entire reload when startup-only resources change. Route and
  default-origin edits remain independent of reload and work offline.
- Keep control replies bounded and errors out of application logs. Only the
  local service UID and root may request a reload; there is no HTTP admin API.

## v0.2.0, pass 2 — concurrent requests and operational lifecycle

Performed after the first pass and its fixes. Reviewed the new snapshot's
ownership through request replay, body buffering, streaming and upgraded
connections; concurrent reload serialization; socket restart/recovery;
systemd DynamicUser permissions; installer updates and fleet rollback.

Findings addressed:

- Share the existing request-body semaphore across configuration snapshots so
  reload cannot reset reservations held by unfinished uploads. Tests keep an
  upload open across reload and verify both budget enforcement and empty GETs.
- Requests retain their old configuration snapshot. End-to-end tests hold a
  video response and a WebSocket open while applying another configuration,
  then verify the same connections still deliver the remaining bytes.
- Lock the control-socket lifecycle across processes before removing a stale
  socket. Reject duplicate instances and only clean up the inode we created.
  Staging and production use separate RuntimeDirectory/socket paths.
- Existing installer users need a managed systemd drop-in for the writable
  runtime directory and ExecReload. Preserve their main unit, restore the
  previous drop-in on failure, and refuse to overwrite an unrelated CLI path.
  Fleet rollback also restores the previous systemd unit with the old binary.
- Correct a race in the upload-budget test: either simultaneous upload can win
  the reservation. The fixture now retries when its unfinished upload loses.
  Ignore Python import caches so test execution leaves release packaging clean.

Verification: rustfmt, Clippy with warnings denied, five Rust unit tests,
the protocol suite, the new management suite, installer boundary fixtures,
Bash syntax and ShellCheck pass locally. Management tests cover offline CRUD,
concurrent writers, preserved metadata/comments, rejected syntax/URL/duplicate
configurations, server-side revalidation, startup-only changes, mismatched
config paths, stale sockets, duplicate instances and uninterrupted media/WS.
The real playback fixture again verifies matching start/seek/tail 64 KiB
responses (206 without Location) and HEAD (200, no body). GitHub repeats the
unit/protocol/management/installer gates on the exact published musl binary
before that release is allowed into the fleet.

## v0.3.0, pass 1 — redirect policy and protocol boundaries

Date: 2026-10-19. Separate in-depth self-review of the completed change; no
independent human or additional-agent review is claimed. Reviewed configuration
parsing, normalization, every redirect transition, response framing, signed
URLs, credential handling, WebSocket rejection and log redaction.

Findings addressed and checked:

- Match only the parsed target hostname, exactly or at a dot-delimited suffix.
  Normalize ASCII case and a single terminal dot. Reject configured URLs,
  wildcard syntax, ports, malformed labels and IP literals, including alternate
  numeric IPv4 notation. Missing/empty lists preserve the previous behavior.
- Apply passthrough only after HTTP(S) and userinfo validation and before
  changing the request method/body or attempting another network request.
  Keep 301/302/303/307/308 status, signed absolute Location and response body;
  HEAD remains bodyless. WebSocket redirects still fail closed.
- Relative and scheme-relative locations cannot be forwarded against the
  client's proxy origin. Resolve them against the upstream. Also resolve
  same-scheme references such as `http:media`, which a standalone URL parser
  can misclassify as absolute. Ordinary absolute URLs remain byte-for-byte
  unchanged, including percent-escape case, duplicate query keys and fragments.
- Count only followed redirects against the existing limit. A response at the
  limit can still be passed through without another hop; continuing to another
  unlisted origin still fails. Cross-origin followed requests keep the existing
  credential allowlist and body-replay rejection. Cross-origin response cookies
  remain stripped, hop headers are removed and no URL is added to logs.

Evidence: six Rust unit tests, format and Clippy with warnings denied pass.
The expanded protocol suite passes root/subdomain matching, signed Location,
GET/HEAD/POST for all five statuses, relative references, cross-origin chains,
invalid scheme/userinfo, redirect limits, streamed 302 first bytes and log
redaction, plus all prior Range/416, backpressure, request-budget, WebSocket,
routing and log-retention checks.

## v0.3.0, pass 2 — reload, playback verification and release lifecycle

Date: 2026-10-19. A separate self-review after pass 1 and its corrections.
Re-read the final config/request code, shared snapshot ownership, CLI/control
paths, stream lifetime, live playback verifier, installer, CI/package scripts
and the private deployment repository's tracked stage/activate/verify/rollback entry points.

Findings addressed and checked:

- The list belongs to each validated configuration snapshot and is reloadable.
  The management suite verifies that disk edits wait for reload, additions and
  empty-list removals take effect, and malformed wildcard updates are rejected
  by both CLI and daemon while the previous policy stays active. Existing
  media/WS continuity and shared upload-budget tests still pass.
- Passthrough uses the existing streaming response path and access-log guard;
  it does not buffer the redirect body, resolve/connect to the excluded host,
  or replay credentials/body to that host. Timeout, cancellation, HEAD framing,
  UTC log rotation and thirty-date retention remain on their existing paths.
- Update the real playback verifier to expect configured client redirects,
  then fetch media using only Range and User-Agent. It still requires 206,
  equal Content-Range and exactly matching 64 KiB start/seek/tail bytes.
  The protocol suite runs this verifier against an actual local redirecting
  origin and proxy, verifies redirected HEAD, and requires failure when the
  expected policy is wrong. Exceptions never print signed URLs or tokens.
- The deployment wrapper reads passthrough domains from its tracked TOML and
  stops on a parsing failure. The release remains pinned by source commit and
  archive SHA256, staged before activation, and rolled back on playback failure.
  Configuration, previous binary and systemd-unit recovery remain paired.

Validation: format, Clippy, all six Rust tests, expanded protocol and management
suites, installer regression fixtures, Python compilation, Bash syntax and
ShellCheck pass. Real playback against the explicitly configured origin with the development
candidate returns matching start/seek/tail 65,536-byte responses, all 206 without
Location, and HEAD 200 without a body. That real fixture uses an unlisted media
origin; external-domain passthrough and signed-address fidelity are covered by the
isolated protocol fixture, not claimed as a real external-provider account playback result.
Private fixture credentials stay outside both repositories. Fleet deployment
must use the exact tagged, pushed revision and successful locked musl CI build;
node-by-node results are recorded in the private deployment repository.

## v0.4.0, pass 1 — follow allowlist and redirect boundaries

Date: 2026-10-19. Separate in-depth self-review, without claiming an independent
human or additional-agent reviewer. Traced configuration deserialization and
normalization, URL parsing, every redirect branch, streamed responses, request
replay and cross-origin credential handling against the new follow-only rule.

Findings addressed and checked:

- Represent an omitted follow list separately from an explicit empty list.
  `redirect_follow_domains = ["example.com"]` follows the root and subdomains only;
  `[]` passes every valid HTTP redirect to the client. Omission retains v0.3
  behavior. Reject an explicit follow list with a nonempty legacy passthrough
  list so a migration cannot silently choose between contradictory policies.
- Share domain validation and exact/dot-boundary matching with the previous
  policy. Reject URL, port, wildcard and IP entries. Case and one terminal dot
  are normalized; `example.net` and `example.com.example.net` cannot match `example.com`.
  Decide from each parsed Location hostname, never from DNS CNAME records.
- Reuse the existing redirect transition and streaming response path. An
  unlisted target is returned before another network request or method/body
  rewrite. Allowed hops retain hop limits, cycle detection, public-address
  checks, credential stripping and cross-origin nonempty-body rejection.
  Absolute signed Location bytes, relative resolution, original status, HEAD,
  Range, backpressure, WebSocket handling and log redaction are preserved.
- Redirect policy uses the hostname in Location, not a DNS alias target.
  Private deployment observations and infrastructure names are omitted from
  this public record; synthetic fixtures cover both forms.

Evidence: format, Clippy with warnings denied and all seven Rust tests pass.
`tests/follow_redirects.py` exercises both decisions for all five supported
redirect statuses, allowed Range/HEAD, exact signed passthrough, no request to
unlisted/alias/lookalike targets, relative signed references, mixed chains,
limits/loops, invalid scheme/userinfo and POST rewrite/replay isolation.
The existing complete protocol suite also passes.

## v0.4.0, pass 2 — reload, verification and deployment gates

Date: 2026-10-19. A second in-depth self-review after pass 1. Re-read the final
configuration and request code, CLI/control reload paths, snapshot ownership,
real-playback verifier, CI packaging and the separate private deployment
entry points, including stage/activate/rollback and their pinned artifacts.

Findings addressed and checked:

- Keep the list in each validated request snapshot. Management regression
  tests verify edits wait for reload, changing the list changes the decision,
  invalid/conflicting policies are rejected by both CLI and daemon without
  changing running behavior, and empty versus omitted lists remain distinct.
  Existing in-flight stream/WebSocket and shared upload-budget checks pass.
- Teach the real-playback verifier the follow-only rule, including an explicit
  empty list. Require the expected client redirect decision before comparing
  206 responses, Content-Range and the exact start/seek/tail 64 KiB bytes.
  Redirected requests carry only Range/User-Agent, and all fetched URLs must
  be HTTP(S) without userinfo. Local integration tests run the verifier in
  follow and passthrough modes and require a wrong expected policy to fail.
- Have rollout read the required follow list from its tracked TOML; omission
  or parse failure aborts, and `[]` remains an explicit verifier argument.
  Verify the configured origin and keep the rollback-on-playback-failure gate.
  Selecting another verifier origin alone cannot change the proxy's origin;
  document that constraint instead of relying on forwarded Host to do so.
- CI now runs the follow-only suite against the actual static musl artifact,
  alongside existing protocol/management tests. Release packaging still uses
  Cargo.lock and the committed source ID; deployment pins the archive digest
  and executes the pulled deployment revision on each target.

Validation: format, Clippy, seven Rust tests, protocol, follow-only, management
and installer suites pass, as do Python compilation, Bash syntax, ShellCheck
and deployment configuration validation. A local candidate using an explicitly configured
alternate origin followed its media redirects and returned matching
start/seek/tail 65,536-byte 206 responses and bodyless HEAD 200. Reloading the candidate to an empty list
returned client redirects and the same media bytes; this also passed.

Production-origin verification is blocked at the time of this review: multiple
indexed samples through the configured production origin redirect to a
media backend which returns 404 even when fetched directly without the Rust
proxy. The alternate-origin result is not recorded as a successful test of the production default route.
Do not activate the fleet until a real playback fixture passes that route.
Private fixtures stay outside both repositories; deployment results and any
remaining gate are recorded in the private deployment repository.

## v0.5.0, pass 1 — explicit configuration and failure paths

Date: 2026-10-21. Separate in-depth self-review of configuration loading,
startup ordering, installer input, CLI validation and daemon-side reload.
No independent human or additional-agent reviewer is claimed.

- Removed the built-in upstream. Missing files, missing keys and blank origins
  fail before logging resources, listening sockets or control sockets start.
  Startup diagnostics go to stderr, which systemd records in the journal.
- Kept the same validation for startup, configuration checks, file management
  and reload. Removing an origin on disk cannot replace a running snapshot.
  Tests for other invalid settings explicitly supply a valid origin so they
  cannot pass accidentally because the required origin is missing.
- Checked interactive blank/whitespace input and noninteractive installation
  without an origin. All fail with a required-configuration message. Explicit
  origins work; update still preserves configuration and rollback behavior.
- Suppressed TOML source excerpts in errors because they can contain private
  URLs or credentials. The example configuration intentionally has no origin,
  and the playback verifier also requires an explicit origin argument.

## v0.5.0, pass 2 — public contents and protocol regressions

Date: 2026-10-21. A second self-review after the first pass and test execution.
Reviewed the complete public file set, README examples, historical evidence,
installer, CI packaging, stream/redirect paths and the history-replacement plan.

- Removed private infrastructure identifiers from code, fixtures, instructions,
  examples and review records. Documentation uses reserved example domains;
  local operator configuration is ignored by Git. Historical playback outcomes
  above remain historical evidence, with infrastructure identifiers redacted.
- Confirmed request routing still uses the explicitly configured origin for
  unknown/missing Host. Redirect policies, signed Location bytes, stream
  backpressure, credential isolation, Range, HEAD and WebSocket handling retain
  their existing paths and pass the protocol suites.
- Inventoried old tags, releases, binaries and Actions artifacts as separate
  copies to remove when replacing Git history. New release binaries must be
  built by CI from the pushed clean revision with Cargo.lock.

Validation: Rust 1.99.0 format check, Clippy with warnings denied, eight Rust
unit tests, debug build, protocol, follow-only, management, startup and installer
suites pass locally. Python compilation, Bash syntax and public-file privacy
scan pass. No fleet deployment or new real-account playback is claimed here;
those remain separate deployment gates.
