# AGENTS.md

Single-crate Rust CLI (`dumbpipe`) that pipes data over iroh (QUIC + hole punching + relay). Not a workspace.

## Layout & architecture

- `src/main.rs` holds the entire CLI: `listen`/`connect` (stdio), `listen-tcp`/`connect-tcp`, `listen-unix`/`connect-unix`, `generate-ticket`. ~950 lines — put subcommand logic here.
- `src/lib.rs` exposes only the wire protocol: `ALPN = b"DUMBPIPEV0"`, `HANDSHAKE = b"hello"`, and re-exports `iroh_base::ticket::NodeTicket`. Bump these carefully — they affect all iroh integrations.
- Handshake protocol: the connect side always writes the 5-byte `HANDSHAKE` first; the listen side reads and validates it. `--custom-alpn` disables the handshake entirely on both sides (see `is_custom_alpn()` in main.rs). ALPN parsing: `utf8:<text>` or hex (main.rs `parse_alpn`).
- iroh is pinned to `0.35` (TLS provider is `ring`, not aws-lc). This repo tracks iroh releases closely — expect API breakages on iroh upgrades.
- The default relay map (when `--relay` is absent) is the `DEFAULT_RELAYS` list of self-hosted chatmail/RU relays in `main.rs`, built into a `RelayMode::Custom` map; iroh's built-in n0 relay preset is not used (the 0.35 client gets HTTP 400 from n0 relays). `--relay <url>` overrides with a single relay.

## Commands (match CI)

```sh
RUSTFLAGS=-Dwarnings cargo fmt --all -- --check
RUSTFLAGS=-Dwarnings cargo clippy --locked --workspace --all-targets --all-features
RUSTFLAGS=-Dwarnings cargo test --locked --workspace --all-features --bins --tests --examples
```

- Integration tests in `tests/cli.rs` spawn the real binary via `env!("CARGO_BIN_EXE_dumbpipe")`, so they require the binary to be built.
- The non-ignored tests make real network calls, always through self-hosted RU relays passed explicitly via `--relay` (trailing-dot FQDN form, listed in `RELAYS`): `dnd.wb.ru`, `chat.gluek.info`, `cm1.wwire.su`. These speak the plain WebSocket relay protocol without `Sec-WebSocket-Protocol` negotiation, which is exactly what the iroh 0.35 relay client sends. Never let tests rely on iroh's built-in default/staging relay maps — the n0 staging relays reject the 0.35 client (HTTP 400) and the RU relays were a deliberate choice. Several tests are `#[ignore = "flaky"]` (connect/listen happy path, listen_tcp, all-relays) — don't un-ignore casually.
- Tests parse the ticket from process output and strip `RUST_LOG`; keep log output off stdout.

## Cross-compile / release binaries

- Release builds happen entirely in GitHub Actions (`release.yml`), triggered on tag `v*` or `workflow_dispatch`. Every target is built in a single matrix job:
  - Desktop: Linux `x86_64-unknown-linux-gnu`, Windows `x86_64-pc-windows-msvc`, macOS `aarch64-apple-darwin` + `x86_64-apple-darwin`.
  - Android: `aarch64-linux-android`, `armv7-linux-androideabi`, `x86_64-linux-android`, `i686-linux-android`, compiled directly against an NDK installed via `nttld/setup-ndk` (r27d, API 24).
- The Android NDK env vars are set inline in the workflow (there is no local build script). Gotchas manual cross-links hit often:
  - Per-target env vars: the cargo linker var is UPPERCASE with underscores (`CARGO_TARGET_ARMV7_LINUX_ANDROIDEABI_LINKER`), but the cc-rs compiler/ar vars are lowercase (`CC_armv7_linux_androideabi`, `AR_armv7_linux_androideabi`). Wrong case is silently ignored — you get a host-`cc` link error or `failed to find tool "arm-linux-androideabi-clang"`.
  - The armv7 NDK clang is `armv7a-linux-androideabi${API}-clang` (note the extra `a`).
  - `ring` builds C asm for Android, so the host needs `perl` plus NDK clang; all four ABIs require their rustup targets installed.
- Release assets are named `dumbpipe-<version>-dumbpipe-<name>.<ext>`; `install-linux.sh` / `install-macos.sh` / `install.ps1` resolve them from the `latest` release, so keep the naming in sync with those scripts.

## Gotchas

- `rand` is pinned to `0.8` in both `[dependencies]` and `[dev-dependencies]` to match iroh 0.35's `rand` 0.8 — do not bump to 0.9/0.10, mixing major versions breaks the build with E0464 ("multiple candidates for rlib dependency").
- `rust-version = "1.91"` in Cargo.toml must stay in sync with the `MSRV` env in `.github/workflows/ci.yml` (per the comment in Cargo.toml).
- Tickets are printed to **stderr**, never stdout — stdout/stdin carry the piped data itself. Tests rely on the ticket being the last thing printed before data flows.
- Runs without `IROH_SECRET` generate a random ephemeral secret; to be reachable by tickets, provide a fixed key via `IROH_SECRET`.
- Release flow: tag `v*` triggers `release.yml`; version-bump commits use the message `chore: Release dumbpipe version X`.