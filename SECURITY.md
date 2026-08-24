# Security Policy

## Supported Versions

Only the latest version on `main` and the latest release are supported. Older versions may no longer receive security fixes.

## Reporting a Vulnerability

Do not disclose an unpatched vulnerability in a public issue, pull request, or discussion. Prefer **Security -> Advisories -> Report a vulnerability** in the GitHub repository to create a private security advisory. If that feature is unavailable, contact `mengshouer` privately through the maintainer's GitHub profile and use `XMic security report` as the message title.

Include:

- the affected version, commit, or installer;
- the Windows version and reproduction steps;
- the expected impact, a minimal reproduction, or relevant logs;
- your preferred contact method and whether you permit public acknowledgment after the issue is fixed.

Do not attach real user audio, access tokens, private keys, or other personal data. We will confirm the affected scope before discussing a fix and disclosure timeline.

## Release Verification

The current release installer is unsigned. The release page identifies it as unsigned and provides `SHA256SUMS.txt`; a GitHub build attestation records build provenance. Verify the installer before running it:

```powershell
Get-FileHash .\XMic_<version>_x64-setup.exe -Algorithm SHA256
```

Compare the result with `SHA256SUMS.txt`. Do not run the installer if the values differ; report the mismatch through a private channel.
