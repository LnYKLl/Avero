# Avero

Avero is a lightweight, open source desktop email client written in Rust. It aims to be fast and frugal with memory, with a native UI (no Electron, no webview), and to handle several IMAP accounts in one unified inbox.

**Status: pre-alpha.** Nothing is usable yet.

Supported platforms: Windows, macOS and Linux.

## Build and run

You need the stable Rust toolchain (https://rustup.rs).

```
cargo run -p avero-app
```

(The `avero-app` crate does not exist yet; it arrives in a later step.)

## Tests

```
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
```

## License

GPL-3.0-or-later. See [LICENSE](LICENSE).
