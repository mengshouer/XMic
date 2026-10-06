export type Language = "zh-CN" | "en";

export const LANGUAGE_STORAGE_KEY = "xmic.language";
export const DEFAULT_LANGUAGE: Language = "zh-CN";

type TranslationShape<T> = T extends (...args: infer Args) => unknown
  ? (...args: Args) => string
  : T extends string
    ? string
    : { [Key in keyof T]: TranslationShape<T[Key]> };

const zhCN = {
  common: {
    language: "界面语言",
    loading: "正在读取音频设备",
    start: "启动",
    stop: "停止",
    applySettings: "应用设置",
    discardSettings: "撤回修改",
    applyBeforeStart: "请先应用待处理的设置更改",
    pendingSettings: "有未应用的设置更改",
    settingsResetPending: "设置已重置，请应用当前默认值",
  },
  app: {
    operationFailed: "操作失败，请稍后重试。",
    lifecycleOperationFailed: "Windows 生命周期操作失败，音频设备已安全释放。",
    sessionUnavailable:
      "Windows 会话当前不可用，麦克风处理已暂停；恢复会话后将按之前状态处理。",
    dspOverrideNotAppliedTitle: "DSP 覆盖未应用",
    dspOverrideNotApplied:
      "XMic 已在运行。请完全退出 XMic 后，再使用 --dsp-library 重试。",
    dismissNotice: "关闭提示",
  },
  navigation: {
    workspace: "工作区",
    monitor: "实时监控",
    routing: "设备与运行",
    processing: "处理链路",
    about: "关于",
  },
  views: {
    monitor: {
      eyebrow: "REAL-TIME ENGINE",
      title: "实时声音监控",
      subtitle: "在一处观察回声消除、语音检测与整条音频链路",
      telemetryTitle: "信号遥测",
      telemetrySubtitle: "最近 72 个状态采样",
      telemetryAria: "VAD 概率与 AEC3 回声衰减时间序列",
      vadLegend: "VAD 概率",
      attenuationLegend: "回声衰减",
      gateWindow: "Gate 开启区间",
      inputPeak: "输入峰值",
      outputPeak: "输出峰值",
      vadProbability: "当前 VAD",
      snapshotTitle: "引擎快照",
      snapshotSubtitle: "当前处理状态",
      pipelineLatency: "额外延迟",
      calibration: "Delay 校准",
      gateState: "VAD Gate",
      echoGuard: "EchoGuard",
      activeRoute: "活动信号路径",
      activeRouteSubtitle: "播放参考与麦克风合流后输出至虚拟设备",
      renderSource: "播放参考",
      captureSource: "物理麦克风",
      processor: "XMic DSP",
      outputDestination: "虚拟麦克风",
      chainTitle: "DSP 模块",
      chainSubtitle: "开关先写入草稿，点击名称进入参数工作台，最后统一应用",
      quickControlsAria: "DSP 模块草稿控制",
      moduleEnabled: "草稿中已启用",
      moduleDisabled: "草稿中已停用",
      alwaysEnabled: "处理输出中始终启用",
      aecQuickDescription: "消除扬声器回声；关闭后可能向通话泄漏回声",
      rnnoiseQuickDescription: "抑制持续环境噪声，并为 VAD 提供语音概率",
      agc2QuickDescription: "稳定说话音量；持续 CPU 过载时整条 DSP 链会进入降级输出",
      vadQuickDescription: "阻断非语音；关闭后底噪和残余回声会直接输出",
      limiterQuickDescription: "约束处理后音频的输出峰值；原始降级输出会绕过它",
      rnnoiseRequiredByVad: "VAD Gate 已启用，请先关闭 VAD Gate",
      vadRequiresRnnoise: "VAD Gate 依赖 RNNoise，请先启用 RNNoise",
      openModule: (module: string) => `打开 ${module} 参数设置`,
      toggleModule: (module: string) => `切换 ${module}`,
      healthTitle: "链路健康",
      healthSubtitle: "本次运行累计数据",
    },
    routing: {
      eyebrow: "DEVICES & RUNTIME",
      title: "设备与运行",
      subtitle: "配置音频设备、故障保护与后台运行方式",
      configuration: "设备配置",
      configurationSubtitle: "输入、参考与输出端点",
      active: "当前活动路由",
      activeSubtitle: "引擎实际使用的端点",
      safety: "降级输出",
      safetySubtitle: "处理异常但链路仍保持开启时的虚拟麦克风内容",
      lifecycle: "后台生命周期",
      lifecycleSubtitle: "登录与启动行为",
    },
    processing: {
      eyebrow: "DSP WORKBENCH",
      title: "处理链路",
      subtitle: "按信号顺序精调回声消除、降噪、增益与语音门限",
      pipelineAria: "DSP 处理模块",
      editorLabel: "当前模块",
      alwaysOn: "处理输出中始终启用",
      diagnostics: "AEC3 运行诊断",
      diagnosticsSubtitle: "Delay、ERLE、VAD 与 EchoGuard 详细数据",
    },
    about: {
      eyebrow: "ABOUT & DIAGNOSTICS",
      title: "关于 XMic",
      subtitle: "查看版本、运行路径、DSP 来源与内嵌许可信息",
    },
  },
  about: {
    overviewTitle: "应用信息",
    overviewSubtitle: "版本与可信来源链接",
    version: "版本",
    unavailable: "不可用",
    mode: "运行模式",
    modes: {
      standard: "Standard",
      portable: "Portable",
    },
    diagnosticsTitle: "运行与 DSP 诊断",
    diagnosticsSubtitle: "当前进程固定的路径、来源与 ABI 校验结果",
    diagnosticsLoading: "正在读取运行诊断…",
    diagnosticsFailed: "无法读取运行诊断。",
    dataDirectory: "数据目录",
    runtimeDirectory: "Runtime 目录",
    webviewDirectory: "WebView2 数据目录",
    dspSource: "DSP 来源",
    dspLibrary: "实际 DSP DLL",
    sources: {
      commandLine: "命令行覆盖",
      executableSidecar: "EXE 同目录覆盖",
      persistentOverride: "持久化覆盖",
      embedded: "内嵌官方 DSP",
    },
    expectedAbi: "预期 ABI",
    actualAbi: "实际 ABI",
    validation: "DSP 校验",
    validationValid: "通过",
    validationFailed: "失败",
    validationPending: "待校验",
    customDspTitle: "Custom DSP",
    customDspBody:
      "当前运行使用用户提供的 native DLL。替换文件后必须完全退出并重新启动 XMic。",
    copyDiagnostics: "复制脱敏诊断",
    copied: "已复制",
    openProject: "项目主页",
    openMplLicense: "MPL-2.0 许可",
    openMplSource: "同版本 MPL 源码包",
    noticesTitle: "许可与 Notice",
    noticesSubtitle: "完整文本仅在点击后从 EXE 内嵌资源加载",
    notices: {
      xmic: {
        title: "XMic MIT License",
        description: "XMic 自有代码许可",
      },
      thirdParty: {
        title: "第三方组件摘要",
        description: "核心第三方组件、许可与归属",
      },
      cargo: {
        title: "Cargo 第三方 Notice",
        description: "锁定 Rust dependency graph 的完整许可文本",
      },
      npm: {
        title: "npm 第三方 Notice",
        description: "前端 runtime dependencies 的完整许可文本",
      },
      webrtc: {
        title: "WebRTC 第三方 Notice",
        description: "内嵌 WebRTC DSP 及其依赖归属",
      },
      mpl: {
        title: "Mozilla Public License 2.0",
        description: "MPL-2.0 完整许可文本",
      },
      mplSources: {
        title: "MPL Source Code 清单",
        description: "同版本外部源码 ZIP 的 package 与 hash 清单",
      },
    },
    noticeLoading: "正在加载许可文本…",
    noticeFailed: "无法加载这份内嵌许可文本。",
    retry: "重试",
    close: "关闭",
    actionFailed: "操作失败",
  },
  engineStates: {
    off: "已关闭",
    processing: "处理中",
    degraded: "降级",
    blocked: "已阻止",
  },
  engineOutputs: {
    inactive: "未激活",
    processed: "处理后音频",
    raw: "原始麦克风",
    silence: "静音",
  },
  primaryReasons: {
    manualOff: "手动关闭",
    starting: "正在启动音频链路",
    aecWarmup: "AEC 正在预热",
    aecRebuild: "AEC 正在重建",
    waitingForPlayback: "正在等待播放参考设备",
    waitingForCapture: "正在等待物理麦克风",
    waitingForVirtualOutput: "正在等待虚拟麦克风设备",
    audioDeviceFailure: "音频设备暂不可用",
    dspFailure: "DSP 暂不可用",
    cpuOverload: "CPU 过载保护",
    safetyConflict: "音频路由存在安全冲突",
    pairUnconfirmed: "自定义虚拟设备对尚未确认",
    sessionUnavailable: "Windows 会话不可用",
    helperUnavailable: "音频引擎不可用",
    crashLoop: "音频引擎连续崩溃，处理已阻止",
    settingsReset: "设置已重置为当前默认值",
    workerTimeout: "音频 worker 响应超时",
  },
  processingLoads: {
    full: "完整",
    protection: "保护",
  },
  status: {
    title: "状态",
    blockedTitle: "处理已阻止",
    output: "当前输出",
    reason: "主要原因",
    retryingIn: (seconds: string) => `约 ${seconds} 秒后重试`,
    noReason: "无",
    endpointUnavailable: "已配置的端点当前不可用",
    dspP99: "DSP 处理耗时 P99",
    dspP99Title: (maximum: string, misses: string) =>
      `最大耗时 ${maximum} µs · ${misses} 次超时`,
    cpuLoad: "CPU 降载模式",
    cpuLoadTitle: (exceeded: string, degradations: string, recoveries: string) =>
      `${exceeded} 次超出预算 · ${degradations} 次降载 · ${recoveries} 次恢复`,
    processedBlocks: "累计处理帧",
    deviceRestarts: "链路重建次数",
    outputQueue: "输出队列延迟",
    outputQueueHelp:
      "虚拟输出队列深度及为补偿独立设备时钟漂移而施加的当前 resampling 修正。",
    audioAnomalies: "音频异常计数",
    audioAnomaliesHelp:
      "播放欠载 / 播放溢出 / 采集溢出 / 输出欠载 / 输出溢出 / 设备丢包 / DSP 故障",
    deviceDiscontinuities: "设备丢包",
    deviceDiscontinuitiesHelp:
      "音频驱动在数据到达 XMic 之前就丢掉的包数。蓝牙等无线设备链路不稳时这个数字会一直涨，属于设备问题，不是 CPU 或 DSP 的问题。",
  },
  routing: {
    title: "设备与运行",
    refresh: "刷新音频设备",
    followDefaultRender: "跟随 Windows 默认播放设备",
    followDefaultRenderHelp:
      "仅在 AEC3 开启时生效：自动使用当前 Windows 默认播放设备作为 AEC reference。切换音响或耳机时 XMic 会跟随重建音频链路；不要把 CABLE Input 设为默认播放设备。",
    renderReference: "AEC 播放参考设备",
    renderReferenceHelp:
      "选择实际正在发声的音响或耳机。AEC3 用它取得远端声音，选错设备会导致回声无法消除。",
    selectRender: "选择播放设备",
    savedRenderUnavailable: "已保存的播放设备当前不可用",
    captureInput: "物理麦克风输入",
    captureInputHelp:
      "选择需要处理的真实麦克风。跟随默认时会随 Windows 默认录音设备切换；不要选择 CABLE Output。",
    followDefaultCapture: "跟随 Windows 默认麦克风",
    selectPhysicalCapture: "选择物理麦克风",
    physicalCaptureRequired: "需要选择物理麦克风",
    physicalCaptureRequiredHelp: (name: string) =>
      name === "-"
        ? "当前虚拟设备配置要求明确选择物理麦克风。请选择实际使用的 3.5 mm 或 USB 麦克风。"
        : `“${name}”是虚拟回传设备或与虚拟路由冲突，不能作为 XMic 的物理输入。请选择实际使用的 3.5 mm 或 USB 麦克风。`,
    windowsDefaultCapture: "Windows 默认麦克风",
    xmicPhysicalCapture: "XMic 物理输入",
    resolvedDefaultCapture: (name: string) => `跟随默认：${name}`,
    notSelected: "未选择",
    savedCaptureUnavailable: "已保存的麦克风当前不可用",
    captureChannel: "麦克风输入声道",
    captureChannelHelp:
      "Auto 会持续比较多通道 USB 麦克风或 audio interface 的输入能量，并采用稳定的有效通道。若自动选择不符合设备接线，可固定为指定 channel。",
    savedChannel: (channel: number) => `Channel ${channel}（已保存）`,
    processedOutput: "处理后虚拟麦克风",
    processedOutputHelp:
      "XMic 将处理后的声音写入此播放端。使用 VB-CABLE 时选择标准 CABLE Input，再让游戏或连麦软件读取对应的 CABLE Output。",
    virtualDeviceMode: "虚拟麦克风类型",
    virtualDeviceModeHelp:
      "标准 VB-CABLE 可自动识别设备对；其他虚拟音频设备需要分别选择写入端和读取端并确认。",
    vbCableMode: "VB-CABLE（推荐）",
    customVirtualMode: "自定义虚拟设备",
    vbCableMissing: "未检测到标准 VB-CABLE",
    vbCableMissingHelp:
      "你可以从官方页面安装 VB-CABLE，也可以继续配置其他具有 render 写入端和 capture 读取端的虚拟音频设备。",
    vbCableWaiting: "正在等待已识别的 VB-CABLE 恢复",
    vbCableWaitingHelp:
      "已识别的 VB-CABLE endpoint pair 当前暂时不可用。XMic 会保留原 endpoint identity，并在设备恢复后自动重试；如果 ID 发生变化，请重新选择并确认。",
    customDeviceActive: "正在使用已确认的自定义虚拟设备",
    customDeviceActiveHelp:
      "未检测到标准 VB-CABLE，但当前自定义设备对已确认。设备在线时 XMic 可以正常使用。",
    customDeviceWaiting: "正在等待自定义虚拟设备恢复",
    customDeviceWaitingHelp:
      "已确认的 endpoint pair 当前不完整。XMic 会保留配置并等待原设备恢复；endpoint ID 变化时需要重新选择和确认。",
    vbCableInstallStepDownload: "从 VB-Audio 官方页面下载并解压 VB-CABLE。",
    vbCableInstallStepAdmin: "运行 x64 Setup，并使用管理员权限完成驱动安装。",
    vbCableInstallStepRefresh: "按安装程序要求重启 Windows，然后返回 XMic 刷新设备。",
    openVbCableDownload: "打开官方下载页",
    copyDeviceDiagnostics: "复制设备诊断",
    diagnosticsCopied: "已复制诊断",
    vbCableDetected: "已检测到完整 VB-CABLE 设备对",
    customVirtualRender: "虚拟设备写入端",
    customVirtualRenderHelp:
      "选择接收 XMic 处理结果的 render/playback endpoint。不要选择实际发声的音响或耳机。",
    selectCustomVirtualRender: "选择虚拟 render endpoint",
    customVirtualCapture: "虚拟设备读取端",
    customVirtualCaptureHelp:
      "选择游戏或语音软件应使用的 capture/microphone endpoint。XMic 不会从此端采集物理人声。",
    selectCustomVirtualCapture: "选择对应的 capture endpoint",
    savedVirtualCaptureUnavailable: "已保存的虚拟读取端当前不可用",
    confirmCustomPair: "我确认这是虚拟音频设备对，不会通过音响或耳机播放声音",
    confirmCustomPairHelp:
      "确认会按当前 endpoint 设备对保存。驱动重装或 endpoint ID 变化后需要重新确认。",
    customPairIncomplete: "请完整选择虚拟设备的写入端和读取端。",
    customPairUnconfirmed: "请确认所选设备是虚拟音频设备对。",
    manageTrustedDevices: "管理已确认设备",
    revokeTrustedDevice: "撤销设备确认",
    selectVirtualOutput: "选择虚拟输出设备",
    selectCableInput: "选择 CABLE Input",
    savedOutputUnavailable: "已保存的虚拟输出当前不可用",
    detectedVirtualDevices: "检测到的虚拟设备",
    otherRenderEndpoints: "其他播放端点",
    activeRouteAria: "当前活动路由",
    currentRoute: "当前路由",
    render: "播放参考",
    capture: "物理麦克风",
    output: "虚拟麦克风",
    dspSettings: "DSP 设置",
    globalDspSettings: "全局 DSP 设置",
    bypassDspSettings: "旁路（仅保留 Limiter）",
    notInUse: "未使用",
    degradedOutput: "降级时的输出",
    degradedOutputHelp:
      "仅在请求处理但因设备、DSP 或 CPU 问题进入降级状态时生效。原始麦克风会绕过包括 Limiter 在内的全部 DSP；静音可避免泄漏声音。XMic 关闭或阻止时虚拟输出未激活，不会使用这里的降级策略。",
    rawMicrophone: "原始麦克风",
    silence: "静音",
    background: "后台与启动",
    backgroundHelp:
      "关闭主窗口后 XMic 会继续驻留系统托盘。锁屏、断开 Windows 会话或睡眠时会暂停处理，并在会话恢复后按之前状态重启。",
    startInTray: "启动时隐藏到托盘",
    startInTrayHelp:
      "XMic 启动时不显示主窗口，仅保留系统托盘图标。关闭后，手动启动或开机启动都会显示主窗口。",
    launchAtLogin: "开机启动",
    launchAtLoginHelp:
      "登录 Windows 时启动 XMic；是否显示主窗口由“启动时隐藏到托盘”控制。关闭后不会删除其他软件的启动项。",
    startOnLaunch: "启动后自动开始处理",
    startOnLaunchHelp:
      "每次 XMic 启动后按已保存的 AEC3、RNNoise 和音频路由配置自动启动实时处理；设备暂不可用时会在后台重试。",
    startOnLaunchBlockedHelp:
      "请先完成物理麦克风和虚拟麦克风配置。已有的自动启动设置仍可关闭，但配置完成前不能新开启。",
    startOnLaunchResetHelp:
      "设置文件已重置为当前默认值。请先在下方 Apply 设置后，再启用自动开始处理。",
    configurationRequired: "需要完成音频设备配置",
    defaultRenderConflict: (name: string) =>
      `Windows 默认播放设备“${name}”同时被选为虚拟麦克风输出。请先将音响或耳机设为 Windows 默认播放设备，然后刷新设备。`,
    renderConflict: (name: string) =>
      `播放参考“${name}”不能同时作为虚拟麦克风输出。请为播放参考选择实际发声的音响或耳机。`,
    physicalCaptureConflict: (name: string) =>
      `物理麦克风“${name}”与虚拟设备冲突。请选择实际使用的 3.5 mm 或 USB 麦克风。`,
  },
  dsp: {
    title: "DSP 链路",
    aecSubtitle: "回声消除",
    rnnoiseSubtitle: "噪声抑制",
    agc2Subtitle: "自动增益控制",
    vadSubtitle: "语音活动门限",
    limiterSubtitle: "输出峰值限制",
    aec: {
      enabled: "启用 AEC3",
      enabledHelp:
        "使用播放设备的声音作为 reference，估计并消除麦克风收到的音响回声。关闭后不会进行 echo cancellation。",
      preset: "AEC3 回声消除强度",
      presetHelp:
        "Natural 优先保留自然音质；Balanced 加强残余回声抑制；Strong 抑制最强，但更可能削弱近端人声。",
      highPass: "AEC3 高通滤波器",
      highPassHelp:
        "滤除麦克风中的低频隆隆声、桌面震动和部分风噪，通常能提高 AEC 稳定性；声音可能略微变薄。",
      diagnosticsAria: "AEC3 运行诊断",
    },
    diagnostics: {
      pipelineLatency: "额外延迟",
      pipelineLatencyHelp:
        "XMic 给麦克风增加的延迟，等于下面采集 / DSP / 输出三段之和。不含 VB-CABLE 驱动内部缓冲与通话软件自身的抖动缓冲——这两段在本机拿不到。",
      captureLatency: "采集侧延迟",
      captureLatencyHelp:
        "实测值：WASAPI 采集包被 worker 取到时已经过了多久（含驱动与端点缓冲），加上排在 DSP 线程前面的积压。取自数据包自带的 QPC 时间戳。",
      dspLatency: "DSP 延迟",
      dspLatencyHelp:
        "确定性值：VAD Gate 的 look-ahead 加 RNNoise 的一帧。完全由 confirmationMs 与各模块开关决定。",
      outputLatency: "输出侧延迟",
      outputLatencyHelp:
        "实测值：软件输出队列 + 唤醒时端点缓冲里未播的帧 + 输出驱动上报的 stream latency。",
      hostDelay: "主机链路延迟",
      hostDelayHelp:
        "根据 capture 与最新已送入 AEC3 的 render QPC 时间戳校准，并作为 stream delay 提供给 AEC3；只表示 WASAPI 队列偏移，不包含 Bluetooth 或声学传播延迟。",
      rawSkew: "原始时钟偏差",
      rawSkewHelp:
        "当前 capture timestamp 减去最新 render timestamp 的原始有符号差值。负值会被钳制为 0 ms，不会作为负 delay 传给 AEC3。",
      hostJitter: "主机延迟抖动",
      hostJitterHelp:
        "Host delay 相对平滑估值的指数移动平均偏差；持续升高通常表示队列时序不稳定。",
      calibration: "延迟校准状态",
      calibrationHelp:
        "等待表示尚无有效时间戳，预热中表示正在收集稳定样本，已校准表示已有可持续使用的 host delay。",
      calibrated: "已校准",
      warming: "预热中",
      waiting: "等待",
      delayResets: "延迟重置次数",
      delayResetsHelp:
        "本次启动中因 WASAPI discontinuity、无效时间戳或大幅时序跳变而重置 delay estimator 与 AEC echo path 的累计次数。",
      aecDelay: "AEC3 估计延迟",
      aecDelayHelp:
        "AEC3 对实际回声路径的独立内部估算，包括音响、Bluetooth 和声学传播；不会反馈成 Host delay，避免重复补偿。",
      erl: "回声回损 ERL",
      erlHelp:
        "Echo Return Loss，表示麦克风收到的回声相对播放 reference 的衰减。",
      erle: "增强回损 ERLE",
      erleHelp:
        "Echo Return Loss Enhancement，表示 AEC3 额外消除的回声量。",
      residualEcho: "残余回声概率",
      residualEchoHelp:
        "WebRTC ResidualEchoDetector 判断当前 AEC 输出仍与播放 reference 相关的概率；仅 Strong 启用。",
      attenuation: "AEC3 回声衰减",
      attenuationHelp:
        "AEC 前后帧能量的平滑比值。负值越小表示该声音被 AEC 判定并衰减得越多。",
      inputPeak: "麦克风输入峰值",
      inputPeakHelp:
        "本次启动以来物理麦克风输入的最高峰值；接近 0 dBFS 且 clipping 增长表示输入端已经削波。",
      outputPeak: "处理后输出峰值",
      outputPeakHelp: "本次启动以来 DSP 最终输出的最高峰值。",
      inputClipping: "输入削波样本",
      inputClippingHelp:
        "物理麦克风输入绝对幅度达到 0.999 以上的样本累计数；持续增长通常表示输入增益过高。",
      limiterSamples: "Limiter 限幅样本",
      limiterSamplesHelp:
        "最终 limiter 实际压低的样本累计数；增长表示 DSP 输出曾接近或超过 ceiling。",
      rnnoiseVad: "RNNoise VAD 概率",
      rnnoiseVadHelp:
        "RNNoise 当前帧的语音概率；启用 AEC3 与 Gate 时，还需要 Speech VAD 同时确认才会打开 Gate。",
      speechVad: "WebRTC Speech VAD",
      speechVadHelp:
        "AEC3 各处理强度下 WebRTC VAD mode 3 的当前判定。只有它与 RNNoise 同时判断为语音才允许开 Gate。",
      speech: "语音",
      noise: "噪声",
      gateState: "VAD Gate 状态",
      gateStateHelp:
        "VAD Gate 当前状态。只播放电脑音频时变为开启，通常表示残余回声触发了 VAD。",
      open: "开启",
      closed: "关闭",
      gateEvents: "VAD Gate 开启次数",
      gateEventsHelp:
        "本次启动以来 VAD Gate 从关闭切换为打开的累计次数。",
      echoGuardState: "EchoGuard 状态",
      echoGuardStateHelp:
        "Strong 模式下的 residual echo 保护。生效时会阻止与播放相关且被 AEC 明显衰减的声音误开 VAD Gate。",
      active: "生效",
      idle: "空闲",
      echoGuardEvents: "EchoGuard 触发次数",
      echoGuardEventsHelp:
        "本次启动以来 Echo guard 从空闲进入生效状态的累计次数。",
      nearendCandidates: "近端语音候选次数",
      nearendCandidatesHelp:
        "Echo guard 生效期间，近端语音候选段开始的累计次数；键盘或残余回声可能产生候选，但不会立刻放行。",
      echoGuardReleases: "EchoGuard 放行次数",
      echoGuardReleasesHelp:
        "近端候选同时满足 VAD、AEC 瞬时/平滑衰减和低回声概率，并连续维持 60 ms 后，Echo guard 实际释放的累计次数；纯播放测试中应为 0。",
    },
    rnnoise: {
      enabled: "启用 RNNoise",
      enabledHelp:
        "使用 RNNoise 抑制风扇、键盘和环境底噪。强噪声下可能损失少量齿音或尾音。",
      mix: "RNNoise 降噪混合比例",
      mixHelp:
        "控制降噪结果的混合比例。0% 为原始声音，100% 为完整 RNNoise 输出；降低比例可换取更自然的音质。",
    },
    agc2: {
      enabled: "启用 AGC2",
      enabledHelp:
        "自动稳定说话音量，让远近和轻重不同的语音保持在较一致的响度，同时受最大增益和 noise limit 约束。",
      adaptive: "AGC2 自适应数字增益",
      adaptiveHelp:
        "根据当前语音响度动态调整 digital gain。关闭后 AGC2 只应用 Fixed gain，不会自动追踪音量；因此 Fixed gain 仍为 0 dB 时不允许关闭它，否则 AGC2 开着却等于直通。",
      advanced: "AGC2 高级参数",
      fixedGain: "AGC2 固定增益",
      fixedGainHelp:
        "始终施加的固定数字增益。提高会让所有声音一起变大，包括底噪；通常保持 0 dB。",
      headroom: "AGC2 输出余量",
      headroomHelp:
        "目标峰值距离 0 dBFS 的安全余量。数值越大越不容易削波，但最终音量也会更低。",
      maxGain: "AGC2 最大增益",
      maxGainHelp:
        "Adaptive digital gain 允许施加的最大增益。过高会把安静环境中的底噪明显放大。",
      initialGain: "AGC2 初始增益",
      initialGainHelp:
        "AGC2 启动时使用的初始增益，之后会逐步调整。接近期望稳态值可减少启动阶段的音量变化。",
      gainSpeed: "AGC2 增益变化速度",
      gainSpeedHelp:
        "每秒允许改变的最大增益。越高响应越快，但更容易出现音量抽动；越低更自然但收敛更慢。",
      noiseLimit: "AGC2 输出噪声上限",
      noiseLimitHelp:
        "限制 AGC2 放大后的估计噪声电平。数值越负限制越严格，可减少底噪抬升，但也可能限制弱语音增益。",
    },
    vad: {
      enabled: "启用 VAD Gate",
      enabledHelp:
        "根据 RNNoise 的语音概率控制 noise gate。无人声时把信号压低到 Floor，减少残余噪声和残余回声。",
      latencyEstimate: "预计 DSP 延迟",
      latencyEstimateHelp:
        "按当前草稿参数算出的 Gate look-ahead + RNNoise。采集侧与输出侧是实测值，运行后在 Monitor 页的额外延迟里看总数。",
      threshold: "VAD 语音检测阈值",
      thresholdHelp:
        "判定为语音所需的最低概率。提高会更积极地关闭噪声，但可能漏掉轻声或句尾；降低则更容易保持开启。",
      confirmation: "VAD 连续确认时长",
      confirmationHelp:
        "VAD 必须连续超过 Threshold 的时间。增加可过滤键盘敲击等短瞬态，但会按 1:1 增加额外延迟，并让短于这个时长的喊话被整句压到 Floor；改动会实时反映在上方的预计额外延迟里。",
      hold: "VAD Gate 保持时长",
      holdHelp:
        "检测不到语音后继续保持 gate 开启的时间。增加可保留短暂停顿和句尾，但会多放出一段环境声。",
      floor: "VAD Gate 关闭电平",
      floorHelp:
        "Gate 关闭时的输出电平。数值越负越安静；-90 dB 接近静音，未通过确认的短句会彻底消失，默认 -20 dB 会把它压低但仍能听出有人在说话。",
      attack: "VAD Gate 开启淡入时长",
      attackHelp:
        "检测到语音后从 Floor 恢复到正常音量的淡入时间。越短起音越完整，过短可能产生突变。",
      release: "VAD Gate 关闭淡出时长",
      releaseHelp:
        "Hold 结束后从正常音量降到 Floor 的淡出时间。越长关闭越自然，但残余噪声持续更久。",
    },
    limiter: {
      ceiling: "Limiter 输出峰值上限",
      ceilingHelp:
        "最终输出允许达到的最高峰值。保持低于 0 dBFS 可防止削波；-1 dBFS 通常兼顾响度和安全余量。",
      release: "Limiter 增益恢复时长",
      releaseHelp:
        "峰值结束后 limiter 恢复正常增益的时间。过短可能产生 pumping，过长会让后续声音暂时偏小。",
    },
  },
} as const;

export type Translation = TranslationShape<typeof zhCN>;

const en = {
  common: {
    language: "Interface language",
    loading: "Reading audio devices",
    start: "Start",
    stop: "Stop",
    applySettings: "Apply settings",
    discardSettings: "Discard changes",
    applyBeforeStart: "Apply pending settings before starting",
    pendingSettings: "Settings have unapplied changes",
    settingsResetPending: "Settings were reset; apply the current defaults",
  },
  app: {
    operationFailed: "The operation failed. Please try again.",
    lifecycleOperationFailed:
      "The Windows lifecycle operation failed; audio devices were released safely.",
    sessionUnavailable:
      "The Windows session is unavailable. Microphone processing is paused and will resume in its previous state when the session returns.",
    dspOverrideNotAppliedTitle: "DSP override was not applied",
    dspOverrideNotApplied:
      "XMic is already running. Fully exit XMic, then try --dsp-library again.",
    dismissNotice: "Dismiss notice",
  },
  navigation: {
    workspace: "Workspace",
    monitor: "Live monitor",
    routing: "Devices & runtime",
    processing: "Processing",
    about: "About",
  },
  views: {
    monitor: {
      eyebrow: "REAL-TIME ENGINE",
      title: "Live audio monitor",
      subtitle: "Observe echo cancellation, voice detection, and the complete audio path",
      telemetryTitle: "Signal telemetry",
      telemetrySubtitle: "Latest 72 status samples",
      telemetryAria: "Time series of VAD probability and AEC3 echo attenuation",
      vadLegend: "VAD probability",
      attenuationLegend: "Echo attenuation",
      gateWindow: "Gate-open window",
      inputPeak: "Input peak",
      outputPeak: "Output peak",
      vadProbability: "Current VAD",
      snapshotTitle: "Engine snapshot",
      snapshotSubtitle: "Current processing state",
      pipelineLatency: "Added latency",
      calibration: "Delay calibration",
      gateState: "VAD Gate",
      echoGuard: "EchoGuard",
      activeRoute: "Active signal path",
      activeRouteSubtitle: "Playback reference and microphone converge before virtual output",
      renderSource: "Playback reference",
      captureSource: "Physical microphone",
      processor: "XMic DSP",
      outputDestination: "Virtual microphone",
      chainTitle: "DSP modules",
      chainSubtitle: "Toggles edit the draft; open a module to tune it, then Apply once",
      quickControlsAria: "DSP module draft controls",
      moduleEnabled: "Enabled in draft",
      moduleDisabled: "Disabled in draft",
      alwaysEnabled: "Always on for processed output",
      aecQuickDescription: "Removes speaker echo; disabling it may leak echo into calls",
      rnnoiseQuickDescription: "Suppresses steady noise and supplies speech probability to VAD",
      agc2QuickDescription: "Stabilizes voice level; sustained CPU overload degrades the entire DSP chain",
      vadQuickDescription: "Blocks non-speech; disabling it passes noise and residual echo",
      limiterQuickDescription: "Constrains processed-output peaks; raw degraded output bypasses it",
      rnnoiseRequiredByVad: "VAD Gate is enabled; disable VAD Gate first",
      vadRequiresRnnoise: "VAD Gate requires RNNoise; enable RNNoise first",
      openModule: (module: string) => `Open ${module} settings`,
      toggleModule: (module: string) => `Toggle ${module}`,
      healthTitle: "Pipeline health",
      healthSubtitle: "Totals for the current run",
    },
    routing: {
      eyebrow: "DEVICES & RUNTIME",
      title: "Devices & runtime",
      subtitle: "Configure audio devices, failure protection, and background behavior",
      configuration: "Device configuration",
      configurationSubtitle: "Input, reference, and output endpoints",
      active: "Current active route",
      activeSubtitle: "Endpoints used by the engine",
      safety: "Degraded output",
      safetySubtitle: "Virtual-microphone output while processing remains on in a degraded state",
      lifecycle: "Background lifecycle",
      lifecycleSubtitle: "Login and launch behavior",
    },
    processing: {
      eyebrow: "DSP WORKBENCH",
      title: "Processing chain",
      subtitle: "Tune echo cancellation, noise suppression, gain, and voice gating in signal order",
      pipelineAria: "DSP processing modules",
      editorLabel: "Selected module",
      alwaysOn: "Always on for processed output",
      diagnostics: "AEC3 runtime diagnostics",
      diagnosticsSubtitle: "Detailed Delay, ERLE, VAD, and EchoGuard telemetry",
    },
    about: {
      eyebrow: "ABOUT & DIAGNOSTICS",
      title: "About XMic",
      subtitle: "View version, runtime paths, DSP source, and embedded license information",
    },
  },
  about: {
    overviewTitle: "Application information",
    overviewSubtitle: "Version and trusted source links",
    version: "Version",
    unavailable: "Unavailable",
    mode: "Runtime mode",
    modes: {
      standard: "Standard",
      portable: "Portable",
    },
    diagnosticsTitle: "Runtime & DSP diagnostics",
    diagnosticsSubtitle: "Paths, source, and ABI validation fixed for this process",
    diagnosticsLoading: "Reading runtime diagnostics…",
    diagnosticsFailed: "Runtime diagnostics could not be read.",
    dataDirectory: "Data directory",
    runtimeDirectory: "Runtime directory",
    webviewDirectory: "WebView2 data directory",
    dspSource: "DSP source",
    dspLibrary: "Actual DSP DLL",
    sources: {
      commandLine: "Command-line override",
      executableSidecar: "EXE sibling override",
      persistentOverride: "Persistent override",
      embedded: "Embedded official DSP",
    },
    expectedAbi: "Expected ABI",
    actualAbi: "Actual ABI",
    validation: "DSP validation",
    validationValid: "Valid",
    validationFailed: "Failed",
    validationPending: "Pending",
    customDspTitle: "Custom DSP",
    customDspBody:
      "This run uses a user-provided native DLL. Fully exit and restart XMic after replacing the file.",
    copyDiagnostics: "Copy redacted diagnostics",
    copied: "Copied",
    openProject: "Project home",
    openMplLicense: "MPL-2.0 license",
    openMplSource: "Matching MPL source archive",
    noticesTitle: "Licenses & notices",
    noticesSubtitle: "Complete text is loaded from the EXE only after an item is opened",
    notices: {
      xmic: {
        title: "XMic MIT License",
        description: "License for XMic-authored code",
      },
      thirdParty: {
        title: "Third-party summary",
        description: "Core third-party components, licenses, and attribution",
      },
      cargo: {
        title: "Cargo third-party notices",
        description: "Complete license text for the locked Rust dependency graph",
      },
      npm: {
        title: "npm third-party notices",
        description: "Complete license text for frontend runtime dependencies",
      },
      webrtc: {
        title: "WebRTC third-party notices",
        description: "Attribution for the embedded WebRTC DSP and its dependencies",
      },
      mpl: {
        title: "Mozilla Public License 2.0",
        description: "Complete MPL-2.0 license text",
      },
      mplSources: {
        title: "MPL Source Code manifest",
        description: "Packages and hashes in the matching external source ZIP",
      },
    },
    noticeLoading: "Loading license text…",
    noticeFailed: "This embedded license text could not be loaded.",
    retry: "Retry",
    close: "Close",
    actionFailed: "Action failed",
  },
  engineStates: {
    off: "Off",
    processing: "Processing",
    degraded: "Degraded",
    blocked: "Blocked",
  },
  engineOutputs: {
    inactive: "Inactive",
    processed: "Processed audio",
    raw: "Raw microphone",
    silence: "Silence",
  },
  primaryReasons: {
    manualOff: "Turned off manually",
    starting: "Starting the audio pipeline",
    aecWarmup: "AEC is warming up",
    aecRebuild: "AEC is rebuilding",
    waitingForPlayback: "Waiting for the playback reference",
    waitingForCapture: "Waiting for the physical microphone",
    waitingForVirtualOutput: "Waiting for the virtual microphone device",
    audioDeviceFailure: "An audio device is unavailable",
    dspFailure: "DSP is unavailable",
    cpuOverload: "CPU overload protection",
    safetyConflict: "The audio route has a safety conflict",
    pairUnconfirmed: "The custom virtual-device pair is unconfirmed",
    sessionUnavailable: "The Windows session is unavailable",
    helperUnavailable: "The audio engine is unavailable",
    crashLoop: "Processing blocked after repeated engine crashes",
    settingsReset: "Settings were reset to current defaults",
    workerTimeout: "An audio worker timed out",
  },
  processingLoads: {
    full: "Full",
    protection: "Protection",
  },
  status: {
    title: "Status",
    blockedTitle: "Processing is blocked",
    output: "Current output",
    reason: "Primary reason",
    retryingIn: (seconds: string) => `Retrying in about ${seconds}s`,
    noReason: "None",
    endpointUnavailable: "The configured endpoint is currently unavailable",
    dspP99: "DSP processing P99",
    dspP99Title: (maximum: string, misses: string) =>
      `Maximum ${maximum} µs · ${misses} deadline misses`,
    cpuLoad: "CPU load mode",
    cpuLoadTitle: (exceeded: string, degradations: string, recoveries: string) =>
      `${exceeded} budget exceedances · ${degradations} degradations · ${recoveries} recoveries`,
    processedBlocks: "Processed blocks",
    deviceRestarts: "Pipeline restarts",
    outputQueue: "Output queue latency",
    outputQueueHelp:
      "Virtual output queue depth and the current resampling correction applied to compensate for drift between independent device clocks.",
    audioAnomalies: "Audio anomaly counts",
    audioAnomaliesHelp:
      "Render underrun / render overflow / capture overflow / output underrun / output overflow / dropped device packets / DSP failure",
    deviceDiscontinuities: "Dropped device packets",
    deviceDiscontinuitiesHelp:
      "Packets the audio driver dropped before XMic ever saw them. This climbs on an unstable wireless link such as Bluetooth; it is a device problem, not CPU or DSP load.",
  },
  routing: {
    title: "Devices & runtime",
    refresh: "Refresh audio devices",
    followDefaultRender: "Follow Windows default playback device",
    followDefaultRenderHelp:
      "Used only while AEC3 is enabled. XMic follows the current Windows default playback device as the AEC reference and rebuilds when it changes. Do not set CABLE Input as the default playback device.",
    renderReference: "AEC playback reference",
    renderReferenceHelp:
      "Select the speakers or headset that are actually playing sound. AEC3 uses this device as the far-end reference; choosing the wrong device prevents echo cancellation.",
    selectRender: "Select playback device",
    savedRenderUnavailable: "Saved playback device is unavailable",
    captureInput: "Physical microphone input",
    captureInputHelp:
      "Select the physical microphone to process. When following the default, this changes with the Windows default recording device. Do not select CABLE Output.",
    followDefaultCapture: "Follow Windows default microphone",
    selectPhysicalCapture: "Select a physical microphone",
    physicalCaptureRequired: "Select a physical microphone",
    physicalCaptureRequiredHelp: (name: string) =>
      name === "-"
        ? "The current virtual-device setup requires an explicit physical microphone. Select the 3.5 mm or USB microphone you use."
        : `“${name}” is a virtual return endpoint or conflicts with the virtual route, so it cannot be used as the physical XMic input. Select the 3.5 mm or USB microphone you use.`,
    windowsDefaultCapture: "Windows default microphone",
    xmicPhysicalCapture: "XMic physical input",
    resolvedDefaultCapture: (name: string) => `Following default: ${name}`,
    notSelected: "Not selected",
    savedCaptureUnavailable: "Saved microphone is unavailable",
    captureChannel: "Microphone input channel",
    captureChannelHelp:
      "Auto continuously compares the input energy of multi-channel USB microphones or audio interfaces and uses a stable active channel. Select a fixed channel if Auto does not match the device wiring.",
    savedChannel: (channel: number) => `Channel ${channel} (saved)`,
    processedOutput: "Processed virtual microphone",
    processedOutputHelp:
      "XMic writes processed audio to this playback endpoint. With VB-CABLE, select the standard CABLE Input, then configure games or voice apps to capture the corresponding CABLE Output.",
    virtualDeviceMode: "Virtual microphone type",
    virtualDeviceModeHelp:
      "XMic detects a standard VB-CABLE pair automatically. Other virtual audio devices require separate write and read endpoints plus confirmation.",
    vbCableMode: "VB-CABLE (recommended)",
    customVirtualMode: "Custom virtual device",
    vbCableMissing: "Standard VB-CABLE was not detected",
    vbCableMissingHelp:
      "Install VB-CABLE from its official page, or configure another virtual audio device that exposes both a render write endpoint and a capture read endpoint.",
    vbCableWaiting: "Waiting for the recognized VB-CABLE pair",
    vbCableWaitingHelp:
      "The recognized VB-CABLE endpoint pair is temporarily unavailable. XMic keeps its endpoint identities and retries automatically; if the IDs changed, select and confirm the pair again.",
    customDeviceActive: "Using a confirmed custom virtual device",
    customDeviceActiveHelp:
      "Standard VB-CABLE is unavailable, but the current custom device pair is confirmed. XMic can run while those endpoints are online.",
    customDeviceWaiting: "Waiting for the custom virtual device",
    customDeviceWaitingHelp:
      "The confirmed endpoint pair is currently incomplete. XMic keeps the configuration and waits for the original devices. Changed endpoint IDs must be selected and confirmed again.",
    vbCableInstallStepDownload:
      "Download and extract VB-CABLE from the official VB-Audio page.",
    vbCableInstallStepAdmin:
      "Run the x64 Setup with administrator privileges to install the driver.",
    vbCableInstallStepRefresh:
      "Restart Windows if requested by the installer, then return to XMic and refresh devices.",
    openVbCableDownload: "Open official download page",
    copyDeviceDiagnostics: "Copy device diagnostics",
    diagnosticsCopied: "Diagnostics copied",
    vbCableDetected: "Complete VB-CABLE device pair detected",
    customVirtualRender: "Virtual device write endpoint",
    customVirtualRenderHelp:
      "Select the render/playback endpoint that receives XMic output. Do not select speakers, headphones, or another physical playback device.",
    selectCustomVirtualRender: "Select a virtual render endpoint",
    customVirtualCapture: "Virtual device read endpoint",
    customVirtualCaptureHelp:
      "Select the capture/microphone endpoint that games and voice apps should use. XMic does not capture your physical voice from this endpoint.",
    selectCustomVirtualCapture: "Select the corresponding capture endpoint",
    savedVirtualCaptureUnavailable:
      "Saved virtual read endpoint is currently unavailable",
    confirmCustomPair:
      "I confirm this is a virtual audio pair and will not play through speakers or headphones",
    confirmCustomPairHelp:
      "Confirmation is saved for the current endpoint pair. Reinstalling the driver or changing endpoint IDs requires confirmation again.",
    customPairIncomplete: "Select both write and read endpoints for the virtual device.",
    customPairUnconfirmed: "Confirm that the selected endpoints form a virtual audio device pair.",
    manageTrustedDevices: "Manage confirmed devices",
    revokeTrustedDevice: "Revoke device confirmation",
    selectVirtualOutput: "Select virtual output device",
    selectCableInput: "Select CABLE Input",
    savedOutputUnavailable: "Saved virtual output is unavailable",
    detectedVirtualDevices: "Detected virtual devices",
    otherRenderEndpoints: "Other playback endpoints",
    activeRouteAria: "Current active route",
    currentRoute: "Current route",
    render: "Playback reference",
    capture: "Physical microphone",
    output: "Virtual microphone",
    dspSettings: "DSP settings",
    globalDspSettings: "Global DSP settings",
    bypassDspSettings: "Bypass (SafetyLimiter only)",
    notInUse: "Not in use",
    degradedOutput: "Output while degraded",
    degradedOutputHelp:
      "Used only when processing is requested but device, DSP, or CPU trouble leaves the engine Degraded. Raw microphone audio bypasses every DSP stage, including the limiter; Silence prevents audio leakage. When XMic is Off or Blocked, its virtual output is inactive and this policy is not used.",
    rawMicrophone: "Raw microphone",
    silence: "Silence",
    background: "Background and startup",
    backgroundHelp:
      "XMic remains in the system tray after the main window closes. Processing pauses while Windows is locked, disconnected, or asleep, then resumes in its previous state when the session returns.",
    startInTray: "Start in system tray",
    startInTrayHelp:
      "Launch XMic without showing the main window, leaving only its system tray icon. When disabled, manual and login launches show the main window.",
    launchAtLogin: "Launch at login",
    launchAtLoginHelp:
      "Start XMic when you sign in to Windows. Main-window visibility follows Start in system tray. Disabling this does not remove startup entries from other applications.",
    startOnLaunch: "Start processing after launch",
    startOnLaunchHelp:
      "Automatically start real-time processing with the saved AEC3, RNNoise, and audio routing settings whenever XMic launches. It retries in the background while devices are unavailable.",
    startOnLaunchBlockedHelp:
      "Finish configuring the physical and virtual microphones first. An existing automatic-start setting can still be disabled, but it cannot be enabled until configuration is complete.",
    startOnLaunchResetHelp:
      "The settings file was reset to current defaults. Apply the settings below before enabling automatic processing.",
    configurationRequired: "Audio device setup required",
    defaultRenderConflict: (name: string) =>
      `The Windows default playback device “${name}” is also selected as the virtual microphone output. Set your speakers or headset as the Windows default playback device, then refresh devices.`,
    renderConflict: (name: string) =>
      `The playback reference “${name}” cannot also be the virtual microphone output. Select the speakers or headset that are actually playing sound as the playback reference.`,
    physicalCaptureConflict: (name: string) =>
      `The physical microphone “${name}” conflicts with the virtual device. Select the 3.5 mm or USB microphone you use.`,
  },
  dsp: {
    title: "DSP Chain",
    aecSubtitle: "Echo cancellation",
    rnnoiseSubtitle: "Noise suppression",
    agc2Subtitle: "Automatic gain control",
    vadSubtitle: "Voice activity gate",
    limiterSubtitle: "Output ceiling",
    aec: {
      enabled: "Enable AEC3",
      enabledHelp:
        "Use playback audio as a reference to estimate and remove speaker echo captured by the microphone. Disabling this turns off echo cancellation.",
      preset: "AEC3 echo cancellation strength",
      presetHelp:
        "Natural prioritizes natural voice quality. Balanced adds stronger residual echo suppression. Strong provides maximum suppression but is more likely to attenuate near-end speech.",
      highPass: "AEC3 high-pass filter",
      highPassHelp:
        "Filter low-frequency rumble, desk vibration, and some wind noise from the microphone. This usually improves AEC stability but may make the voice slightly thinner.",
      diagnosticsAria: "AEC3 runtime diagnostics",
    },
    diagnostics: {
      pipelineLatency: "Added latency",
      pipelineLatencyHelp:
        "Latency XMic adds to the microphone: the sum of the capture, DSP, and output segments below. Excludes VB-CABLE's internal buffering and the voice app's own jitter buffer, neither of which is observable from this process.",
      captureLatency: "Capture latency",
      captureLatencyHelp:
        "Measured: how stale a WASAPI capture packet already is when the worker receives it (driver and endpoint buffering included), plus any backlog queued ahead of the DSP thread. Taken from the QPC timestamp each packet carries.",
      dspLatency: "DSP latency",
      dspLatencyHelp:
        "Deterministic: VAD Gate look-ahead plus one RNNoise frame. Decided entirely by confirmationMs and which stages are enabled.",
      outputLatency: "Output latency",
      outputLatencyHelp:
        "Measured: software output queue + frames still unplayed in the endpoint buffer at wake-up + the stream latency the output driver reports.",
      hostDelay: "Host pipeline delay",
      hostDelayHelp:
        "Calibrated from capture and latest AEC3 render QPC timestamps, then supplied to AEC3 as stream delay. It represents only WASAPI queue offset, excluding Bluetooth and acoustic propagation delay.",
      rawSkew: "Raw clock skew",
      rawSkewHelp:
        "The signed difference between the current capture timestamp and latest render timestamp. Negative values are clamped to 0 ms and are never passed to AEC3 as a negative delay.",
      hostJitter: "Host delay jitter",
      hostJitterHelp:
        "Exponential moving average of the deviation from smoothed host delay. Sustained increases usually indicate unstable queue timing.",
      calibration: "Delay calibration",
      calibrationHelp:
        "Waiting means no valid timestamps are available, Warming means stable samples are being collected, and Calibrated means a sustainable host delay is available.",
      calibrated: "Calibrated",
      warming: "Warming",
      waiting: "Waiting",
      delayResets: "Delay resets",
      delayResetsHelp:
        "Number of times the delay estimator and AEC echo path were reset during this run due to WASAPI discontinuities, invalid timestamps, or large timing jumps.",
      aecDelay: "AEC3 estimated delay",
      aecDelayHelp:
        "AEC3's independent estimate of the actual echo path, including speakers, Bluetooth, and acoustic propagation. It is not fed back into Host delay, avoiding duplicate compensation.",
      erl: "Echo return loss (ERL)",
      erlHelp:
        "Echo Return Loss: attenuation of the echo captured by the microphone relative to the playback reference.",
      erle: "ERL enhancement (ERLE)",
      erleHelp:
        "Echo Return Loss Enhancement: the additional echo attenuation applied by AEC3.",
      residualEcho: "Residual echo likelihood",
      residualEchoHelp:
        "The WebRTC ResidualEchoDetector estimate that AEC output is still correlated with the playback reference. Enabled only in Strong mode.",
      attenuation: "AEC3 echo attenuation",
      attenuationHelp:
        "Smoothed ratio of frame energy before and after AEC. More negative values mean AEC classified and attenuated more of the sound.",
      inputPeak: "Microphone input peak",
      inputPeakHelp:
        "Highest physical microphone input peak during this run. Values near 0 dBFS with a rising clipping count indicate input clipping.",
      outputPeak: "Processed output peak",
      outputPeakHelp: "Highest final DSP output peak during this run.",
      inputClipping: "Input clipped samples",
      inputClippingHelp:
        "Number of physical microphone samples with absolute amplitude at or above 0.999. Sustained growth usually means the input gain is too high.",
      limiterSamples: "Limiter-reduced samples",
      limiterSamplesHelp:
        "Number of samples actually reduced by the final limiter. Growth means DSP output approached or exceeded the ceiling.",
      rnnoiseVad: "RNNoise VAD probability",
      rnnoiseVadHelp:
        "Current RNNoise speech probability. When AEC3 and Gate are enabled, Speech VAD must also confirm speech before the Gate opens.",
      speechVad: "WebRTC Speech VAD",
      speechVadHelp:
        "Current WebRTC VAD mode 3 decision for the selected AEC3 strength. Both it and RNNoise must classify speech before the Gate can open.",
      speech: "Speech",
      noise: "Noise",
      gateState: "VAD Gate state",
      gateStateHelp:
        "Current VAD Gate state. If it opens while only computer audio is playing, residual echo probably triggered the VAD.",
      open: "Open",
      closed: "Closed",
      gateEvents: "VAD Gate open events",
      gateEventsHelp:
        "Number of transitions from closed to open during this run.",
      echoGuardState: "EchoGuard state",
      echoGuardStateHelp:
        "Residual echo protection used in Strong mode. Active prevents playback-correlated audio that AEC strongly attenuated from opening the VAD Gate.",
      active: "Active",
      idle: "Idle",
      echoGuardEvents: "EchoGuard activation events",
      echoGuardEventsHelp:
        "Number of EchoGuard transitions from Idle to Active during this run.",
      nearendCandidates: "Near-end speech candidates",
      nearendCandidatesHelp:
        "Number of near-end speech candidate segments started while EchoGuard was Active. Keyboard noise or residual echo may create candidates but is not released immediately.",
      echoGuardReleases: "EchoGuard release events",
      echoGuardReleasesHelp:
        "Number of candidates released after simultaneously meeting VAD, instantaneous and smoothed AEC attenuation, and low echo probability requirements for 60 ms. This should remain 0 in playback-only tests.",
    },
    rnnoise: {
      enabled: "Enable RNNoise",
      enabledHelp:
        "Use RNNoise to suppress fans, keyboard noise, and ambient background noise. Strong noise may cost a small amount of sibilance or word endings.",
      mix: "RNNoise suppression mix",
      mixHelp:
        "Set the mix of noise-suppressed audio. 0% is the original signal and 100% is full RNNoise output. Lower values trade suppression for a more natural sound.",
    },
    agc2: {
      enabled: "Enable AGC2",
      enabledHelp:
        "Automatically stabilize voice volume across different distances and speaking levels, constrained by maximum gain and the noise limit.",
      adaptive: "AGC2 adaptive digital gain",
      adaptiveHelp:
        "Dynamically adjust digital gain based on current speech level. When disabled, AGC2 applies only Fixed gain and does not track volume, so it cannot be turned off while Fixed gain is still 0 dB — that combination would leave AGC2 enabled but transparent.",
      advanced: "Advanced AGC2 parameters",
      fixedGain: "AGC2 fixed gain",
      fixedGainHelp:
        "Digital gain applied at all times. Increasing it raises every sound, including background noise. Normally leave this at 0 dB.",
      headroom: "AGC2 output headroom",
      headroomHelp:
        "Safety margin between the target peak and 0 dBFS. Higher values reduce clipping risk but also lower final volume.",
      maxGain: "AGC2 maximum gain",
      maxGainHelp:
        "Maximum gain allowed for Adaptive digital gain. High values can noticeably amplify background noise in quiet environments.",
      initialGain: "AGC2 initial gain",
      initialGainHelp:
        "Gain used when AGC2 starts before it gradually adapts. A value near the expected steady state reduces startup volume changes.",
      gainSpeed: "AGC2 gain change speed",
      gainSpeedHelp:
        "Maximum gain change per second. Higher values respond faster but can cause volume pumping; lower values sound more natural but converge slowly.",
      noiseLimit: "AGC2 output noise limit",
      noiseLimitHelp:
        "Limit the estimated noise level after AGC2 amplification. More negative values impose a stricter limit, reducing raised background noise but potentially limiting quiet speech gain.",
    },
    vad: {
      enabled: "Enable VAD Gate",
      enabledHelp:
        "Use RNNoise speech probability to control the noise gate. When there is no speech, reduce the signal to Floor to suppress residual noise and echo.",
      latencyEstimate: "Estimated DSP latency",
      latencyEstimateHelp:
        "Gate look-ahead plus RNNoise, computed from the current draft. The capture and output segments are measured instead — see Added latency on the Monitor page for the total once the engine runs.",
      threshold: "VAD speech threshold",
      thresholdHelp:
        "Minimum probability required to classify speech. Higher values close more aggressively but may miss quiet speech or word endings; lower values stay open more easily.",
      confirmation: "VAD confirmation time",
      confirmationHelp:
        "Time VAD must remain above Threshold. Increasing it filters short transients such as keyboard strikes, but it adds the same amount of latency and drops any call-out shorter than this window down to Floor. The estimate above updates as you change it.",
      hold: "VAD Gate hold time",
      holdHelp:
        "Keep the Gate open for this duration after speech is no longer detected. Higher values preserve short pauses and word endings but pass more ambient sound.",
      floor: "VAD Gate closed level",
      floorHelp:
        "Output level while the Gate is closed. More negative values are quieter; -90 dB is nearly silent, so an unconfirmed short utterance disappears completely, while the -20 dB default keeps it audible but attenuated.",
      attack: "VAD Gate attack time",
      attackHelp:
        "Fade from Floor to normal volume after speech is detected. Shorter times preserve speech onset; excessively short times may produce an abrupt transition.",
      release: "VAD Gate release time",
      releaseHelp:
        "Fade from normal volume to Floor after Hold ends. Longer times close more naturally but retain residual noise for longer.",
    },
    limiter: {
      ceiling: "Limiter output ceiling",
      ceilingHelp:
        "Highest permitted final output peak. Keep this below 0 dBFS to prevent clipping; -1 dBFS usually balances loudness and safety margin.",
      release: "Limiter gain recovery time",
      releaseHelp:
        "Time for limiter gain to recover after a peak. Too short may cause pumping; too long temporarily reduces following audio.",
    },
  },
} satisfies Translation;

export const translations: Record<Language, Translation> = {
  "zh-CN": zhCN,
  en,
};

export function readStoredLanguage(): Language {
  try {
    const value = window.localStorage.getItem(LANGUAGE_STORAGE_KEY);
    return value === "en" || value === "zh-CN" ? value : DEFAULT_LANGUAGE;
  } catch {
    return DEFAULT_LANGUAGE;
  }
}

export function storeLanguage(language: Language): void {
  try {
    window.localStorage.setItem(LANGUAGE_STORAGE_KEY, language);
  } catch {
    // A denied storage write should not prevent an in-memory language change.
  }
}
