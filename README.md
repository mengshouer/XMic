# XMic

XMic 是一款本机运行的 Windows 麦克风优化工具。它从物理麦克风和当前播放设备采集音频，在实时处理后将声音输出到虚拟麦克风，适用于使用音响通话、游戏语音和日常连麦。

## 功能

- 使用 WebRTC AEC3 消除音响回声，并自动跟随 Windows 默认播放设备。
- 使用 RNNoise 降低键盘、风扇和环境底噪。
- 提供 Natural、Balanced 和 Strong 三种 AEC 处理强度。
- 提供可调节的 VAD Gate、AGC2、High-pass Filter 和 Limiter。
- 支持 Windows 默认麦克风、指定麦克风以及多通道 USB 麦克风。
- 在播放设备或麦克风切换后自动重建音频链路。
- 按播放设备和麦克风组合保存独立 DSP Profile，并在设备切换时自动应用。
- 音频处理运行在独立 helper process；异常退出后自动重启，连续崩溃时进入静音安全模式。
- 持续 CPU 过载时优先降低可选处理，必要时临时静音，并在负载稳定后自动恢复。
- 支持系统托盘后台运行，并可快速开关 AEC、AGC2、VAD Gate 及切换 AEC 处理强度。
- 支持开机启动以及启动后自动开始处理。
- 锁屏、断开 Windows 会话或睡眠时暂停处理，并在会话恢复后按原状态重启。
- 所有音频均在本机处理，软件运行时不需要联网。

## 使用要求

- Windows 10 22H2 或 Windows 11。
- 单独安装 VB-CABLE，并将 XMic 的虚拟麦克风输出设置为 `CABLE Input`。
- 在游戏或通话软件中选择对应的 `CABLE Output` 作为麦克风。

XMic 不会修改 Windows 默认播放设备。使用自动播放参考时，实际发声的音响或耳机应保持为 Windows 默认播放设备。
