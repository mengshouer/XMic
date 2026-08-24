# XMic

**English** | [简体中文](README.zh-CN.md)

XMic is a fully local Windows microphone processing application. It captures audio from a physical microphone and the active playback device, processes it in real time, and sends the result to a virtual microphone. It is designed for speakerphone calls, in-game voice chat, and everyday voice communication.

## Screenshot

![XMic processing chain](docs/images/xmic-processing-chain-en.png)

## Features

- Uses WebRTC AEC3 to cancel speaker audio picked up by the microphone before it reaches other participants.
- Reduces keyboard, fan, and ambient noise with RNNoise.
- Provides Natural, Balanced, and Strong AEC presets.
- Includes configurable VAD Gate, AGC2, High-pass Filter, and Limiter stages.
- Supports selected microphones and multichannel USB microphones.
- Automatically detects standard VB-CABLE installations by default and supports manually confirmed render/capture endpoint pairs from other virtual audio devices.
- Rebuilds the audio pipeline after the playback device or microphone changes.
- Stores an independent DSP Profile for each playback-device and microphone pair and applies it automatically when devices change.
- Runs audio processing in a separate helper process, restarts it after unexpected exits, and enters safe mode after repeated crashes.
- Uses one “Output while not processing” setting to decide what the virtual microphone carries when XMic is inactive: raw microphone audio or silence. The same setting applies before processing starts, after startup failures, during DSP failures, in safe mode, and under CPU overload protection.
- Reduces optional processing during sustained CPU overload, falls back according to “Output while not processing” when necessary, and restores processing after the load stabilizes.
- Sends one system notification per run when speech is detected while processing is inactive. The tray icon also turns gray, and its tooltip reports whether listeners receive raw microphone audio or silence.
- Supports background operation from the system tray, with quick controls for AEC, AGC2, VAD Gate, and AEC preset selection.
- Supports launch at sign-in and optional automatic processing after startup.
- Pauses processing and releases audio devices while Windows is locked, disconnected, or sleeping, then restores the previous state when the session resumes. Raw microphone audio is not forwarded while suspended.
- Processes all audio locally and requires no network connection at runtime.

## Requirements

- Tested on Windows 10 and Windows 11.
- A separately installed [VB-CABLE](https://vb-audio.com/Cable/) is recommended. XMic does not download, install, or bundle a virtual audio driver.
- With a standard VB-CABLE installation, XMic automatically pairs the `CABLE Input` render endpoint with the `CABLE Output` capture endpoint.
- Other virtual audio devices require manually selecting and confirming their render/capture endpoint pair. The pair must be confirmed again if a driver reinstall changes its endpoint IDs.
- Select the virtual device's capture/microphone endpoint as the microphone in games and calling applications.

XMic does not change the default Windows playback device or microphone. When automatic playback reference is enabled, keep the speakers or headphones that actually produce sound as the default Windows playback device. If the virtual microphone is the default Windows microphone, explicitly select the physical microphone in XMic to avoid a feedback route.

## Building from Source

Development and packaging require Windows, Node.js 24, Rust 1.97.0, Visual Studio 2022 C++ tools, and a Windows SDK. Node and Rust versions are pinned by `.node-version` and `rust-toolchain.toml`. The repository includes a verified WebRTC DSP DLL, so normal builds do not download or compile WebRTC.

```powershell
npm ci
npm run bundle
```

For frontend development, run `npm run build` or `npm run tauri dev`. `npm run bundle` is the only release-packaging entry point: it checks version consistency, generates license resources from the locked Cargo and npm dependency graphs, verifies the checked-in WebRTC artifact, and builds the NSIS installer with Cargo lockfile enforcement. Ordinary `cargo check`, `cargo test`, and `cargo clippy` commands do not depend on generated license resources.

Rebuilding WebRTC is only necessary after changing `native/webrtc-audio-processing/`, changing the WebRTC build recipe, or updating the WebRTC revision. It additionally requires PowerShell 7, Windows SDK 10.0.26100.0, Debugging Tools for Windows, and at least 20 GB of free disk space:

```powershell
.\scripts\bootstrap-webrtc.ps1
.\scripts\build-webrtc.ps1 -Jobs 2
npm run artifacts:check
```

The WebRTC revision is pinned in `native/webrtc-audio-processing/webrtc-lock.json`. Rebuilding the DLL also generates its third-party notice and `PROVENANCE.json`. `npm run artifacts:check` verifies the DLL, notice, provenance, overlay sources, and lockfile hashes. Missing or stale license text, required source bundles, notices, or WebRTC artifacts block release packaging.

Common checks:

```powershell
npm test
npm run build
cargo test --workspace --locked
npm run bundle
```

See [CONTRIBUTING.md](CONTRIBUTING.md) for the complete contribution and validation workflow.

## License

XMic is licensed under the [MIT License](LICENSE). See [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md) for third-party attribution. Complete dependency notices are included in release packages.

The current Windows installer is unsigned. Release pages provide `SHA256SUMS.txt` and an unsigned-binary warning, while a GitHub build attestation records build provenance. Verify the SHA-256 checksum before running the installer, and expect Windows SmartScreen to report an unknown publisher.
