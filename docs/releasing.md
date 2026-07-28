# Releasing Lios

GitHub Actions is the only producer of official release artifacts. Files created under a
developer's local `target/` directory are smoke-test outputs and must not be uploaded as a
formal release.

## Version source of truth

The authoritative version lives in the root `Cargo.toml`:

```toml
[workspace.package]
version = "0.1.0"
```

Every Rust workspace package uses `version.workspace = true`. `Cargo.lock`, `package.json`,
`package-lock.json`, and `src-tauri/tauri.conf.json` are synchronized mirrors for their build
tools.

```bash
npm run version:sync
npm run version:check
```

The release workflow also requires the pushed tag to equal `v` plus the workspace version.

## Release matrix

One semantic version tag creates one GitHub Release. Asset prefixes keep desktop and CLI
deliverables distinct.

Current desktop assets:

```text
lios-desktop-vX.Y.Z-windows-x86_64-nsis.exe
lios-desktop-vX.Y.Z-linux-x86_64.deb
lios-desktop-vX.Y.Z-linux-x86_64.rpm
lios-desktop-vX.Y.Z-linux-x86_64.AppImage
```

Current headless CLI assets:

```text
lios-cli-vX.Y.Z-linux-x86_64.deb
lios-cli-vX.Y.Z-linux-x86_64.rpm
```

Official CLI publishing is intentionally limited to DEB and RPM packages for Linux x86_64. The
release does not contain a CLI tarball, a bare `lios` binary, or ARM64/aarch64 CLI packages.
Desktop and CLI executable names are intentionally separate: Windows and Linux Desktop packages
install `lios-desktop` (`lios-desktop.exe` on Windows), while only the separately packaged CLI may
own `/usr/bin/lios`. Desktop installers do not add their installation directory to `PATH`.

The pre-1.0 executable rename does not support an in-place upgrade from the old `lios` Desktop
binary. Uninstall the old 0.1.x Desktop package before installing the renamed build. Uninstalling
the application must leave the user's `~/.lios` configuration, tasks, credentials, staging state,
logs, and recovery key intact.

## Current unsigned release policy

The first release series intentionally does not apply Lios-controlled package signatures:

- The Windows NSIS installer, `lios-desktop.exe`, and uninstaller have no Lios Authenticode
  signature. Windows can therefore display an Unknown publisher or SmartScreen warning. The
  bundled Microsoft `WebView2Loader.dll` retains its upstream Microsoft signature.
- Desktop and CLI RPMs have no OpenPGP package signature.
- Releases do not contain Sigstore bundles or GitHub artifact attestations.
- `SHA256SUMS` covers the six versioned Lios packages. The workflow checks it immediately after
  aggregation and again after the publish job downloads the final artifact set. GitHub-generated
  source archives and `SHA256SUMS` itself are not listed in that file.

The workflow does not read any release-signing secrets and does not require a GitHub Environment.
Signing will be introduced together in a future version. Existing unsigned Release assets must
never be replaced in place with signed rebuilds; publish a new semantic version instead.

## Cutting a release

Repository setup is part of the release boundary:

- Protect the `v*` tag namespace with a GitHub tag ruleset or protection rule.
- Keep the default branch protected. A release tag is rejected unless its commit is already an
  ancestor of the repository's default branch.

1. Change `[workspace.package].version` in the root `Cargo.toml`.
2. Run `npm run version:sync`.
3. Run the full local verification suite and commit the version change.
4. Create and push the matching tag, for example `v0.2.0`.
5. The `Unsigned release` workflow tests, builds, inspects, checksums, and finally creates the
   GitHub Release.

The Windows job performs a silent current-user install into the real default location,
`%LOCALAPPDATA%\Lios`, without overriding it with a temporary test path. It starts the installed
Desktop long enough to confirm that it remains running, then stops and uninstalls it before
publishing. That smoke test confirms the Lios-produced PE files are unsigned, validates the
upstream WebView2 loader signature, confirms the reserved `lios.exe` CLI name is absent, and
confirms the current-user `PATH` remains unchanged through install and uninstall. Before
publication, the workflow also confirms that the release tag still resolves to the exact commit
that triggered the run.

The explicit bundle publisher metadata, `Lingbou`, gives NSIS a stable registry namespace for the
remembered install location. It is package metadata only: it does not sign the executable or remove
the Windows Unknown publisher warning.

The CLI job builds only the `lios` binary, then creates a DEB and RPM. It extracts/inspects both
packages before publication: the package name must be `lios-cli`, the architecture must be
amd64/x86_64, `/usr/bin/lios` must be executable, and `/usr/bin/lios-desktop` must be absent. It
also rejects missing libc/OpenSSL runtime dependencies, unresolved shared libraries, or a packaged
CLI that cannot print its help text. The inverse path checks run on Desktop packages so the two
products cannot silently claim the same path.

Third-party Actions are pinned to full commit SHAs. Package build jobs cannot publish a Release;
the checksum aggregation job has read-only repository permissions, and only the final publish job
receives `contents: write`.

Linux packages are built on Ubuntu 22.04 for an older glibc baseline. Native Desktop and CLI RPM
inspection runs with a digest-pinned Fedora 42 container. It validates package digests, paths,
architecture, and CLI runtime dependencies without requiring an OpenPGP key.

The workflow intentionally creates an unsigned Release only after every build, package inspection,
smoke test, exact asset-set check, and checksum check succeeds. A failed job leaves no GitHub
Release behind.

## Consumer verification

Download `SHA256SUMS` and verify the six versioned packages from the same Release:

```bash
sha256sum --check SHA256SUMS
```

Checksums detect an incomplete or corrupted download but, without an independent signature, do not
prove publisher identity. Windows and RPM users should expect the platform to report that the Lios
package is unsigned. Only install assets downloaded directly from the repository's GitHub Release.
