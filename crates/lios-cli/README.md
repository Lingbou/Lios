# Lios CLI

`lios-cli` is the headless command-line edition of Lios. Its installed binary is named `lios`;
the Cargo package remains `lios-cli` so Desktop and CLI release assets stay distinct.

Build it from the repository root:

```bash
cargo build -p lios-cli --release
```

Run it without installing:

```bash
cargo run -p lios-cli -- --help
```

## First use

Run `lios setup`. Lios creates the local state directory and recovery key, then asks for the
ModelScope token with hidden terminal input. Run `lios auth` later to replace the saved token.

The first packaged release targets Linux. On Linux, the token is stored as plaintext in a local
credentials file whose permissions are restricted to the current user (`0600`). The CLI does not
use an environment-variable token path.

The code also uses Windows DPAPI when built on Windows and shares `%USERPROFILE%\.lios` with
Desktop, but a Windows CLI package is not part of the first release.

Use global `--home DIR` for an isolated `DIR/.lios` state directory.

## Commands

```text
lios setup
lios auth
lios status [--remote]
lios repos list
lios repos create --namespace NAME --dataset NAME
lios space open --namespace NAME --dataset NAME
lios space init --namespace NAME --dataset NAME
lios ls [/PATH]
lios search QUERY
lios mkdir --parent NODE_ID NAME
lios rename NODE_ID NEW_NAME
lios upload --parent NODE_ID PATH...
lios download --output DIR NODE_ID...
lios delete NODE_ID...
lios verify [--full]
lios task list
lios task resume TASK_ID
```

This first release is designed for people at an interactive terminal. It deliberately has no JSON
output mode. Upload conflicts and destructive deletes require an interactive choice.

All catalog and transfer operations call `lios-application`, the same Tauri-independent service
layer used by Desktop. Upload, download, delete, verify, and task resume run the durable task in the
foreground and do not report success merely because work was queued.

## Concurrency boundary

On one computer, Desktop and CLI share `~/.lios`; an operating-system lock serializes writes to the
same space. This is local-process protection, not distributed locking. ModelScope does not provide
the atomic parent-commit compare-and-swap needed to make concurrent writers on different computers
safe. In the first release, only one computer may write to a given space at a time.

## Linux packages

`Cargo.toml` contains `cargo-deb` and `cargo-generate-rpm` metadata. After building the release
binary, package with:

```bash
cargo deb -p lios-cli --no-build
cargo generate-rpm -p crates/lios-cli
```

The resulting package installs `/usr/bin/lios` and the CLI README.
