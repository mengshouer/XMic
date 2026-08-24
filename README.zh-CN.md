# XMic

[English](README.md) | **简体中文**

XMic 是一款纯本地运行的 Windows 麦克风处理工具。它从物理麦克风和当前播放设备采集音频，在实时处理后将声音输出到虚拟麦克风，适用于使用音响通话、游戏语音和日常连麦。

## 软件截图

![XMic 处理链路](docs/images/xmic-processing-chain-zh-CN.png)

## 功能

- 使用 WebRTC AEC3 消除音响回声，能够实现麦克风接收到的音响声音在对方听到时被消除。
- 使用 RNNoise 降低键盘、风扇和环境底噪。
- 提供 Natural、Balanced 和 Strong 三种 AEC 处理强度。
- 提供可调节的 VAD Gate、AGC2、High-pass Filter 和 Limiter。
- 支持指定麦克风以及多通道 USB 麦克风。
- 默认自动识别标准 VB-CABLE，并支持手动确认其他具有 render/capture endpoint pair 的虚拟音频设备。
- 在播放设备或麦克风切换后自动重建音频链路。
- 按播放设备和麦克风组合保存独立 DSP Profile，并在设备切换时自动应用。
- 音频处理运行在独立 helper process；异常退出后自动重启，连续崩溃时进入安全模式。
- 通过「未处理时的输出」统一决定 XMic 没在处理时虚拟麦克风的内容：输出原始麦克风声音，或保持静音。未启动处理、启动失败、DSP 故障、安全模式和 CPU 过载保护都遵循该设置。
- 持续 CPU 过载时优先降低可选处理，必要时按「未处理时的输出」退化，并在负载稳定后自动恢复。
- 没在处理时若检测到你在说话，会发送一条系统通知；每次运行只提示一次。托盘图标同时会变灰，tooltip 会说明对方当前听到的是原始麦克风还是静音。
- 支持系统托盘后台运行，并可快速开关 AEC、AGC2、VAD Gate 及切换 AEC 处理强度。
- 支持开机启动以及启动后自动开始处理。
- 锁屏、断开 Windows 会话或睡眠时暂停处理并释放音频设备（此时不会输出原始麦克风声音），并在会话恢复后按原状态重启。
- 所有音频均在本机处理，软件运行时不需要联网。

## 使用要求

- 在 Windows 10 / Windows 11 上测试通过。
- 推荐单独安装 [VB-CABLE](https://vb-audio.com/Cable/)。XMic 不会下载、安装或捆绑虚拟音频驱动。
- 使用标准 VB-CABLE 时，XMic 会自动匹配 `CABLE Input` 写入端和 `CABLE Output` 读取端。
- 使用其他虚拟音频设备时，需要在 XMic 中手动选择写入端和读取端，并确认该 endpoint pair。驱动重装导致 endpoint ID 变化后需要重新确认。
- 在游戏或通话软件中选择虚拟设备的 capture/microphone 读取端作为麦克风。

XMic 不会修改 Windows 默认播放设备或默认麦克风。使用自动播放参考时，实际发声的音响或耳机应保持为 Windows 默认播放设备。若将虚拟麦克风设为 Windows 默认麦克风，需要在 XMic 中明确选择实际使用的物理麦克风，避免形成回授路由。

## 从源码构建

开发和打包需要 Windows、Node.js 24、Rust 1.97.0、Visual Studio 2022 C++ tools 以及 Windows SDK。Node 和 Rust 版本分别由 `.node-version`、`rust-toolchain.toml` 固定。仓库已经包含经过校验的 WebRTC DSP DLL；普通构建不需要下载或编译 WebRTC。

```powershell
npm ci
npm run bundle
```

前端开发可以只运行 `npm run build` 或 `npm run tauri dev`。`npm run bundle` 是唯一的 release packaging 入口：它会校验版本一致性，根据锁定的 Cargo 和 npm dependency graph 生成许可资源，检查仓库中的 WebRTC artifact，并在 Cargo lockfile 保护下构建 NSIS installer。普通 `cargo check`、`cargo test` 和 `cargo clippy` 不依赖这些 generated license resources。

只有修改 `native/webrtc-audio-processing/`、WebRTC build recipe 或升级 WebRTC revision 时，才需要额外准备 PowerShell 7、Windows SDK 10.0.26100.0、Debugging Tools for Windows 和至少 20 GB 可用空间，然后执行：

```powershell
.\scripts\bootstrap-webrtc.ps1
.\scripts\build-webrtc.ps1 -Jobs 2
npm run artifacts:check
```

WebRTC 版本由 `native/webrtc-audio-processing/webrtc-lock.json` 固定。构建 WebRTC DLL 时会同时生成对应的第三方许可清单和 `PROVENANCE.json`；`npm run artifacts:check` 会校验 DLL、notice、provenance、overlay source 和 lock 中的 SHA-256。缺少或过期的 license text、required source bundle、notice 或 WebRTC artifact 会阻止 release packaging。

常用检查：

```powershell
npm test
npm run build
cargo test --workspace --locked
npm run bundle
```

完整的贡献和检查流程见 [CONTRIBUTING.md](CONTRIBUTING.md)。

## 许可证

XMic 使用 [MIT License](LICENSE)。第三方组件的归属说明见 [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md)，完整依赖许可清单会随 release package 一同分发。

当前 Windows installer 未进行 code signing。发布页会提供 `SHA256SUMS.txt` 和 unsigned 提示，GitHub build attestation 记录构建来源；运行前请先核对 SHA-256，并注意 Windows SmartScreen 可能显示未知发布者。
