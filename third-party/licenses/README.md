# Third-Party License Overrides

These files fill license-text omissions in specific locked Cargo packages.
`scripts/generate-third-party-notices.mjs` maps each file to exact package
versions and records its upstream source. Every override is pinned by a
SHA-256 hash declared in `scripts/generate-third-party-notices.mjs` (the
`overrideLicenseHashes` map); changing the text without updating the
corresponding hash fails the check. An unlisted version still fails the license
check instead of inheriting an old override.

The generator parses SPDX expressions instead of guessing from arbitrary text.
It normalizes only an explicit allowlist of legacy Cargo slash expressions,
including `MIT/Apache-2.0`; unknown slash expressions fail validation. All
declared license paths and copied source trees must remain inside their package
root after resolving symlinks.

These files retain their upstream licenses and are not covered by XMic's MIT
License. Generated notices and MPL Source Code Form are written below the
ignored `artifacts/licenses/` directory during `npm run licenses:generate`.
The current MPL policy keeps source trees as directories; if the generated
bundle exceeds 1,000 files or 20 MiB, generation fails and the packaging policy
must be reviewed before switching to an archive format.
