import type { EngineOutput, EngineState, PrimaryReasonCode } from "./engineStatus";
import type { ConfigurationIssue, VirtualDeviceMode } from "./routeValidation";

export type RuntimeMode = "standard" | "portable";

export type DspLibrarySource =
  | "commandLine"
  | "executableSidecar"
  | "persistentOverride"
  | "embedded";

export interface ApplicationDiagnostics {
  mode: RuntimeMode;
  dataDirectory: string;
  runtimeDirectory: string;
  webviewDataDirectory: string;
  dspSource: DspLibrarySource;
  dspCustom: boolean;
  dspLibraryPath: string;
  dspLibraryFileName: string;
  dspSelectionPinned: boolean;
  dspSelectionGuardError: string | null;
  expectedDspAbi: number;
  actualDspAbi: number | null;
  dspValidationError: string | null;
}

export type DspValidationState = "valid" | "failed" | "pending";

export interface DspValidationSnapshot {
  expectedAbi: number;
  actualAbi: number | null;
  error: string | null;
}

export interface EngineDiagnosticSnapshot {
  desiredEnabled: boolean;
  state: EngineState;
  output: EngineOutput;
  primaryReason: {
    code: PrimaryReasonCode;
    detail: string | null;
    endpointId: string | null;
    retryAfterMs: number | null;
    configurationIssue: ConfigurationIssue | null;
  } | null;
  dspValidation?: DspValidationSnapshot;
  settings: {
    followDefaultRender: boolean;
    renderEndpointId: string | null;
    captureEndpointId: string | null;
    captureChannel: number | null;
    outputEndpointId: string | null;
    virtualDeviceMode: VirtualDeviceMode | null;
    degradedOutput: "raw" | "silence";
    dsp: {
      aec: {
        enabled: boolean;
        preset: "natural" | "balanced" | "strong";
        highPassFilterEnabled: boolean;
      };
      rnnoise: {
        enabled: boolean;
        mix: number;
      };
      agc2: {
        enabled: boolean;
        adaptiveDigitalEnabled: boolean;
        fixedGainDb: number;
        headroomDb: number;
        maxGainDb: number;
      };
      vadGate: {
        enabled: boolean;
        threshold: number;
        confirmationMs: number;
        holdMs: number;
        floorDb: number;
      };
      limiter: {
        gainDb?: number;
        ceilingDbfs: number;
        releaseMs: number;
      };
    };
  };
  activeRoute: {
    renderEndpointId: string | null;
    captureEndpointId: string | null;
    outputEndpointId: string;
  } | null;
  metrics: {
    processedBlocks: number;
    renderUnderruns: number;
    renderOverflows: number;
    captureOverflows: number;
    deviceDiscontinuities: number;
    outputUnderruns: number;
    outputQueueOverflows: number;
    outputQueueDepthMs: number;
    outputRateAdjustmentPpm: number;
    deviceRestarts: number;
    dspFailures: number;
    processingTimeP99Us: number;
    processingTimeMaxUs: number;
    processingDeadlineMisses: number;
    processingBudgetExceededBlocks: number;
    processingOverloadEvents: number;
    processingRecoveryEvents: number;
    processingLoadMode: "full" | "protection";
    captureLatencyMs: number;
    dspLatencyMs: number;
    outputLatencyMs: number;
    pipelineLatencyMs: number;
    hostDelayMs: number | null;
    hostDelayCalibrationState: "uncalibrated" | "warmingUp" | "calibrated";
    hostDelayResetEvents: number;
    aecEstimatedDelayMs: number | null;
    aecEchoReturnLossDb: number | null;
    aecEchoReturnLossEnhancementDb: number | null;
    aecResidualEchoLikelihood: number | null;
    inputPeakDbfs: number | null;
    outputPeakDbfs: number | null;
    inputClippedSamples: number;
    limiterLimitedSamples: number;
    vadProbability: number | null;
    echoGuardActive: boolean | null;
    echoGuardActiveEvents: number;
  };
}

/**
 * Mirrors `DspLibraryValidation::is_valid()`: only an ABI readback that matches the expected
 * version is valid. A missing readback means the helper never answered — that is pending, not
 * valid — and a mismatching readback is a failure even without a message.
 */
export function dspValidationState(
  diagnostics: ApplicationDiagnostics,
): DspValidationState {
  if (diagnostics.dspValidationError || diagnostics.dspSelectionGuardError) {
    return "failed";
  }
  if (diagnostics.actualDspAbi == null) return "pending";
  return diagnostics.actualDspAbi === diagnostics.expectedDspAbi
    ? "valid"
    : "failed";
}

function formatConfigurationIssue(issue: ConfigurationIssue | null): string | null {
  if (!issue) return null;
  switch (issue.type) {
    case "physicalCaptureRequired":
      return "physicalCaptureRequired";
    case "virtualDeviceRequired":
      return `virtualDeviceRequired(${issue.reason})`;
    case "virtualRouteConflict":
      return `virtualRouteConflict(${issue.kind})`;
  }
}

function formatPrimaryReason(
  reason: EngineDiagnosticSnapshot["primaryReason"],
): string {
  if (!reason) return "none";
  const parts: string[] = [reason.code];
  const issue = formatConfigurationIssue(reason.configurationIssue);
  if (issue) parts.push(`issue=${issue}`);
  if (reason.retryAfterMs != null) parts.push(`retryAfterMs=${reason.retryAfterMs}`);
  return parts.join(", ");
}

function formatOptNumber(
  value: number | null | undefined,
  unit = "",
  digits = 0,
): string {
  if (value == null || !Number.isFinite(value)) return "n/a";
  return `${value.toFixed(digits)}${unit}`;
}

export function mergeLiveDspValidation(
  diagnostics: ApplicationDiagnostics,
  engine?: EngineDiagnosticSnapshot | null,
): ApplicationDiagnostics {
  const live = engine?.dspValidation;
  if (!live) return diagnostics;
  const liveResolved = live.actualAbi != null || live.error != null;
  const currentResolved =
    diagnostics.actualDspAbi != null || diagnostics.dspValidationError != null;
  if (!liveResolved && currentResolved) {
    return diagnostics;
  }
  return {
    ...diagnostics,
    expectedDspAbi: live.expectedAbi,
    actualDspAbi: live.actualAbi,
    dspValidationError: live.error,
  };
}

export function buildRedactedApplicationDiagnostics(
  diagnostics: ApplicationDiagnostics,
  version: string | null,
  engine?: EngineDiagnosticSnapshot | null,
): string {
  const effectiveDiagnostics = mergeLiveDspValidation(diagnostics, engine);
  const lines = [
    `XMic version: ${version ?? "unknown"}`,
    `Mode: ${effectiveDiagnostics.mode}`,
    `DSP source: ${effectiveDiagnostics.dspSource}`,
    `Custom DSP: ${effectiveDiagnostics.dspCustom ? "yes" : "no"}`,
    `DSP library file: ${effectiveDiagnostics.dspLibraryFileName || "unknown"}`,
    `Expected DSP ABI: ${effectiveDiagnostics.expectedDspAbi}`,
    `Actual DSP ABI: ${effectiveDiagnostics.actualDspAbi ?? "unavailable"}`,
    `DSP validation: ${dspValidationState(effectiveDiagnostics)}`,
  ];

  if (!engine) {
    return lines.join("\n");
  }

  const { settings, activeRoute, metrics } = engine;
  const { dsp } = settings;
  const renderConfig = settings.followDefaultRender
    ? "followDefault"
    : settings.renderEndpointId
      ? "explicit"
      : "unset";
  const captureConfig = settings.captureEndpointId ? "explicit" : "followDefault";
  const channelConfig =
    settings.captureChannel == null ? "all" : `ch${settings.captureChannel}`;
  const virtualMode = settings.virtualDeviceMode ?? "vbCable";
  const activeRouteSummary = activeRoute
    ? `render=${activeRoute.renderEndpointId ? "bound" : "none"}, capture=${activeRoute.captureEndpointId ? "bound" : "none"}, output=bound`
    : "inactive";
  const limiterGain = dsp.limiter.gainDb ?? 0;
  const limiterGainPart =
    limiterGain === 0
      ? ""
      : `gain=${limiterGain > 0 ? `+${limiterGain}` : limiterGain}dB,`;
  const limiterSummary = `${limiterGainPart}${dsp.limiter.ceilingDbfs}dBFS/${dsp.limiter.releaseMs}ms`;

  lines.push(
    `Engine state: ${engine.state} (desired=${engine.desiredEnabled ? "on" : "off"}, output=${engine.output})`,
    `Primary reason: ${formatPrimaryReason(engine.primaryReason)}`,
    `Route config: render=${renderConfig}, capture=${captureConfig}(${channelConfig}), virtual=${virtualMode}, degradedOutput=${settings.degradedOutput}`,
    `Active route: ${activeRouteSummary}`,
    `DSP chain: AEC3=${dsp.aec.enabled ? `on(${dsp.aec.preset}, hpf=${dsp.aec.highPassFilterEnabled ? "on" : "off"})` : "off"} | RNNoise=${dsp.rnnoise.enabled ? `on(mix=${dsp.rnnoise.mix.toFixed(2)})` : "off"} | AGC2=${dsp.agc2.enabled ? `on(adaptive=${dsp.agc2.adaptiveDigitalEnabled ? "on" : "off"}, fixed=${dsp.agc2.fixedGainDb}dB, headroom=${dsp.agc2.headroomDb}dB, max=${dsp.agc2.maxGainDb}dB)` : "off"} | VAD=${dsp.vadGate.enabled ? `on(thr=${dsp.vadGate.threshold.toFixed(2)}, confirm=${dsp.vadGate.confirmationMs}ms, hold=${dsp.vadGate.holdMs}ms, floor=${dsp.vadGate.floorDb}dB)` : "off"} | Limiter=${limiterSummary}`,
    `Load & timing: mode=${metrics.processingLoadMode}, p99=${metrics.processingTimeP99Us}us, max=${metrics.processingTimeMaxUs}us, deadlineMisses=${metrics.processingDeadlineMisses}, budgetExceeded=${metrics.processingBudgetExceededBlocks}, overloads=${metrics.processingOverloadEvents}, recoveries=${metrics.processingRecoveryEvents}`,
    `Latency: total=${formatOptNumber(metrics.pipelineLatencyMs, "ms", 1)} (capture=${formatOptNumber(metrics.captureLatencyMs, "ms", 1)}, dsp=${formatOptNumber(metrics.dspLatencyMs, "ms", 1)}, output=${formatOptNumber(metrics.outputLatencyMs, "ms", 1)}), queueDepth=${formatOptNumber(metrics.outputQueueDepthMs, "ms", 1)}, rateAdjust=${formatOptNumber(metrics.outputRateAdjustmentPpm, "ppm", 0)}`,
    `Stream health: blocks=${metrics.processedBlocks}, captureOverflows=${metrics.captureOverflows}, renderOverflows=${metrics.renderOverflows}, renderUnderruns=${metrics.renderUnderruns}, outputUnderruns=${metrics.outputUnderruns}, outputQueueOverflows=${metrics.outputQueueOverflows}, discontinuities=${metrics.deviceDiscontinuities}, restarts=${metrics.deviceRestarts}, dspFailures=${metrics.dspFailures}`,
    `Signal & AEC3: hostDelay=${formatOptNumber(metrics.hostDelayMs, "ms", 0)}(${metrics.hostDelayCalibrationState}, resets=${metrics.hostDelayResetEvents}), aecDelay=${formatOptNumber(metrics.aecEstimatedDelayMs, "ms", 0)}, erl=${formatOptNumber(metrics.aecEchoReturnLossDb, "dB", 1)}, erle=${formatOptNumber(metrics.aecEchoReturnLossEnhancementDb, "dB", 1)}, residualEcho=${formatOptNumber(metrics.aecResidualEchoLikelihood, "", 2)}, echoGuard=${metrics.echoGuardActive == null ? "n/a" : metrics.echoGuardActive ? "active" : "idle"}(events=${metrics.echoGuardActiveEvents}), vadProb=${formatOptNumber(metrics.vadProbability, "", 2)}, inputPeak=${formatOptNumber(metrics.inputPeakDbfs, "dBFS", 1)}, outputPeak=${formatOptNumber(metrics.outputPeakDbfs, "dBFS", 1)}, clipped=${metrics.inputClippedSamples}, limited=${metrics.limiterLimitedSamples}`,
  );

  return lines.join("\n");
}
