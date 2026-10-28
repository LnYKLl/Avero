# Avero

Avero is a lightweight, open source desktop email client written in Rust. It aims to be fast and frugal with memory, with a native UI (no Electron, no webview), and to handle several IMAP accounts in one unified inbox.

**Status: pre-alpha.** Nothing is usable yet.

Supported platforms: Windows, macOS and Linux.

## Build and run

You need the stable Rust toolchain (https://rustup.rs).

```
cargo run -p avero-app
```

The default build draws the UI in software (tiny-skia), which keeps memory use low. To use the GPU renderer (wgpu) instead, build with `--features gpu`.

## Known limitations (pre-alpha)

- Accounts cannot be edited or deleted yet. To remove a broken account, close Avero, delete the `avero.db` file (plus `avero.db-wal` and `avero.db-shm` if present) in the app data folder, and delete the `avero` / `account-N` entries in the OS credential manager (Windows Credential Manager, macOS Keychain, or the Secret Service on Linux). The app data folder is:
  - Windows: `%APPDATA%\Avero\Avero\data`
  - macOS: `~/Library/Application Support/org.Avero.Avero`
  - Linux: `~/.local/share/avero`
- Flag changes and deletions made in another mail client are not synced yet; only new messages are fetched.
- Outlook.com / Hotmail consumer accounts need OAuth2, which is not supported yet. Use an IMAP app password (for example with Gmail) or a provider that allows password login.
- The message list refreshes only when an account's whole sync finishes; there is no periodic refresh yet.

## Tests

```
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
```

## License

GPL-3.0-or-later. See [LICENSE](LICENSE).
