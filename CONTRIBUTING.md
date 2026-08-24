# Contributing to XMic

Thank you for contributing to XMic. Before submitting a change, confirm that it is within the project's scope and do not commit personal environment files, build caches, or unaudited binaries.

## Development Environment

XMic targets Windows 10 22H2 and Windows 11. After installing the required tools, run the following commands from the repository root:

```powershell
npm ci
cargo test --workspace --locked
npm test
```

Use the Node.js version in `.node-version` and the Rust version in `rust-toolchain.toml`. The repository includes the WebRTC DSP DLL required for ordinary builds. Run `scripts/bootstrap-webrtc.ps1` and `scripts/build-webrtc.ps1` with PowerShell 7 only when changing the native WebRTC code, its build recipe, or its pinned revision. Do not commit the WebRTC checkout.

## Pre-Submission Checks

Run at least:

```powershell
cargo fmt --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
npm test
npm run bundle
```

`npm run bundle` checks version consistency, generates license resources, verifies the checked-in WebRTC artifact, and builds the NSIS installer. Generated license resources are written under the ignored `artifacts/licenses/` directory; do not copy them into the repository manually. Release and scheduled Security workflows audit all npm and Cargo dependencies.

## Licenses and Dependencies

XMic-authored code is licensed under MIT. When adding or upgrading a dependency, review its SPDX expression, distributable license text, and whether its license requires Source Code Form to accompany the executable. Commit lockfile updates together with dependency changes. `scripts/generate-third-party-notices.mjs` generates the license notices; do not edit generated output manually.

The WebRTC DLL is distributed as a checked-in Windows artifact. After changing `native/webrtc-audio-processing/`, the WebRTC build recipe, or the pinned revision, rerun `scripts/build-webrtc.ps1` and commit a matching `webrtc-lock.json`, notice, `PROVENANCE.json`, and DLL. Provenance must record the source revisions, GN arguments, SDK version, tool versions, and overlay hashes, and `npm run artifacts:check` must pass.

## Pull Requests

Describe behavioral changes, validation commands, and any Windows-only validation limitations in the pull request. Keep commits small and focused. If a change affects release packaging, dependencies, or licensing, state the affected scope explicitly. CI performs a complete NSIS packaging smoke test on Windows; merge only after it passes.

## Privacy and Vulnerabilities

Respect the project's privacy and security boundaries: XMic processes audio locally and does not require network access at runtime. Do not disclose security vulnerabilities in a public issue. Follow [SECURITY.md](SECURITY.md) to report them privately.
