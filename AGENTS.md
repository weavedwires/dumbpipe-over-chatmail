# AGENTS.md

Single-crate Rust CLI (`dumbpipe`) that pipes data over iroh (QUIC + hole punching + relay). Not a workspace.

## Layout & architecture

- `src/main.rs` holds the entire CLI: `listen`/`connect` (stdio), `listen-tcp`/`connect-tcp`, `listen-unix`/`connect-unix`, `generate-ticket`. 900 lines — put subcommand logic here.
- `src/lib.rs` exposes only the wire protocol: `ALPN = b"DUMBPIPEV0"`, `HANDSHAKE = b"hello"`, and re-exports `iroh_tickets::endpoint::EndpointTicket`. Bump these carefully — they affect all iroh integrations.
- Handshake protocol: the connect side always writes the 5-byte `HANDSHAKE` first; the listen side reads and validates it. `--custom-alpn` disables the handshake entirely on both sides (see `is_custom_alpn()` in main.rs). ALPN parsing: `utf8:<text>` or hex (main.rs `parse_alpn`).
- iroh is pinned to `1.0.0` (feature `tls-ring`). This repo tracks iroh releases closely — expect API breakages on iroh upgrades.

## Commands (match CI)

```sh
IROH_FORCE_STAGING_RELAYS=1 RUSTFLAGS=-Dwarnings cargo fmt --all -- --check
IROH_FORCE_STAGING_RELAYS=1 RUSTFLAGS=-Dwarnings cargo clippy --locked --workspace --all-targets --all-features
IROH_FORCE_STAGING_RELAYS=1 RUSTFLAGS=-Dwarnings cargo test --locked --workspace --all-features --bins --tests --examples
```

- Integration tests in `tests/cli.rs` spawn the real binary via `env!("CARGO_BIN_EXE_dumbpipe")`, so they require the binary to be built.
- The non-ignored tests still make real network calls through an iroh relay. Set `IROH_FORCE_STAGING_RELAYS=1` (as CI does) or they may hang/fail. Several tests are `#[ignore = "flaky"]` (connect/listen happy path, listen_tcp) — don't un-ignore casually.
- Tests parse the ticket from process output and strip `RUST_LOG`; keep log output off stdout.

## Gotchas

- `rust-version = "1.91"` in Cargo.toml must stay in sync with the `MSRV` env in `.github/workflows/ci.yml` (per the comment in Cargo.toml).
- Tickets are printed to **stderr**, never stdout — stdout/stdin carry the piped data itself. Tests rely on the ticket being the last thing printed before data flows.
- Runs without `IROH_SECRET` generate a random ephemeral secret; to be reachable by tickets, provide a fixed key via `IROH_SECRET`.
- Release flow: tag `v*`, GitHub Actions cross-compiles (musl/aarch64/…). Version-bump commits use the message `chore: Release dumbpipe version X`.