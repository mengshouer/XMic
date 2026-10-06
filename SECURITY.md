# Security Policy

## Supported Versions

Only the latest version on `main` and the latest release are supported. Older versions may no longer receive security fixes.

## Reporting a Vulnerability

Do not disclose an unpatched vulnerability in a public issue, pull request, or discussion. Prefer **Security -> Advisories -> Report a vulnerability** in the GitHub repository to create a private security advisory. If that feature is unavailable, contact `mengshouer` privately through the maintainer's GitHub profile and use `XMic security report` as the message title.

Include:

- the affected version, commit, installer, standalone executable, or portable archive;
- the Windows version and reproduction steps;
- the expected impact, a minimal reproduction, or relevant logs;
- your preferred contact method and whether you permit public acknowledgment after the issue is fixed.

Do not attach real user audio, access tokens, private keys, or other personal data. We will confirm the affected scope before discussing a fix and disclosure timeline.

## Release Verification

Every Windows payload in a release is unsigned: the NSIS installer, the standalone `XMic.exe`, and `XMic_x64-portable.zip`. The release page identifies them as unsigned and provides `SHA256SUMS.txt`; a GitHub build attestation records build provenance. Verify each payload before running it:

```powershell
Get-FileHash .\XMic_<version>_x64-setup.exe -Algorithm SHA256
Get-FileHash .\XMic.exe -Algorithm SHA256
Get-FileHash .\XMic_x64-portable.zip -Algorithm SHA256
Get-FileHash .\XMic_<version>_MPL-2.0-source.zip -Algorithm SHA256
```

Compare each result with `SHA256SUMS.txt`. Do not run a payload if its value differs; report the mismatch through a private channel.

A replacement `xmic_webrtc_dsp.dll` selected through `--dsp-library`, next to the executable, or in an `overrides/` directory is arbitrary native code. XMic verifies neither its checksum nor its signature, so only use libraries you built or trust; XMic never downloads one by itself.
