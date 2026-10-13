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
The real Iris playback fixture again verifies matching start/seek/tail 64 KiB
responses (206 without Location) and HEAD (200, no body). GitHub repeats the
unit/protocol/management/installer gates on the exact published musl binary
before that release is allowed into the fleet.
