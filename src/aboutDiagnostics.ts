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
    hostDelayRawSkewMs?: number | null;
    hostDelayJitterMs?: number | null;
    hostDelayCalibrationState: "uncalibrated" | "warmingUp" | "calibrated";
    hostDelayResetEvents: number;
    aecEstimatedDelayMs: number | null;
    aecEchoReturnLossDb: number | null;
    aecEchoReturnLossEnhancementDb: number | null;
    aecResidualEchoLikelihood: number | null;
    aecAttenuationDb?: number | null;
    renderPeakDbfs?: number | null;
    inputPeakDbfs: number | null;
    aecOutputPeakDbfs?: number | null;
    outputPeakDbfs: number | null;
    renderSampleRateHz?: number | null;
    renderChannels?: number | null;
    captureSampleRateHz?: number | null;
    captureChannels?: number | null;
    renderKeepaliveActive?: boolean | null;
    renderDriftPpm?: number | null;
    captureDriftPpm?: number | null;
    renderQueueBlocks?: number;
    captureQueueBlocks?: number;
    renderWaitTimeouts?: number;
    captureWaitTimeouts?: number;
    inputClippedSamples: number;
    limiterLimitedSamples: number;
    vadProbability: number | null;
    speechVadActive?: boolean | null;
    vadGateOpen?: boolean | null;
    vadGateOpenEvents?: number;
    echoGuardActive: boolean | null;
    echoGuardActiveEvents: number;
    echoGuardNearendCandidateEvents?: number;
    echoGuardReleaseEvents?: number;
  };
}

export const MAX_DEBUG_TIMELINE_SAMPLES = 180;
export const MAX_DEBUG_TIMELINE_EVENTS = 100;

export interface DebugTimelineSample {
  seq: number;
  elapsedSec: number;
  state: EngineState;
  output: EngineOutput;
  reasonCode: PrimaryReasonCode | null;
  renderPeakDbfs: number | null;
  inputPeakDbfs: number | null;
  aecOutputPeakDbfs: number | null;
  outputPeakDbfs: number | null;
  hostDelayMs: number | null;
  rawSkewMs: number | null;
  jitterMs: number | null;
  calibrationState: "uncalibrated" | "warmingUp" | "calibrated";
  aecEstimatedDelayMs: number | null;
  erlDb: number | null;
  erleDb: number | null;
  residualEchoLikelihood: number | null;
  aecAttenuationDb: number | null;
  vadProbability: number | null;
  speechVadActive: boolean | null;
  vadGateOpen: boolean | null;
  echoGuardActive: boolean | null;
  renderDriftPpm: number | null;
  captureDriftPpm: number | null;
  renderQueueBlocks: number;
  captureQueueBlocks: number;
  processedBlocks: number;
  renderUnderruns: number;
  deviceDiscontinuities: number;
  hostDelayResetEvents: number;
  deviceRestarts: number;
}

export interface DebugTimelineEvent {
  seq: number;
  elapsedSec: number;
  kind:
    | "stateChange"
    | "delayReset"
    | "discontinuity"
    | "deviceRestart"
    | "queueOverflow"
    | "echoGuard";
  summary: string;
}

export interface DebugTraceState {
  enabled: boolean;
  startedAtMs: number | null;
  nextSeq: number;
  samples: DebugTimelineSample[];
  events: DebugTimelineEvent[];
}

export type DebugFindingCode =
  | "noRenderSignal"
  | "aecLowAttenuation"
  | "delayResets"
  | "discontinuities"
  | "healthy";

export function createDebugTraceState(enabled = false): DebugTraceState {
  return {
    enabled,
    startedAtMs: null,
    nextSeq: 1,
    samples: [],
    events: [],
  };
}

export function recordDebugSnapshot(
  trace: DebugTraceState,
  engine: EngineDiagnosticSnapshot,
  nowMs = Date.now(),
): DebugTraceState {
  if (!trace.enabled) return trace;
  const startedAtMs = trace.startedAtMs ?? nowMs;
  const elapsedSec = Math.max(0, Math.round((nowMs - startedAtMs) / 1000));
  const seq = trace.nextSeq;
  const { metrics } = engine;
  const reasonCode = engine.primaryReason?.code ?? null;

  const sample: DebugTimelineSample = {
    seq,
    elapsedSec,
    state: engine.state,
    output: engine.output,
    reasonCode,
    renderPeakDbfs: metrics.renderPeakDbfs ?? null,
    inputPeakDbfs: metrics.inputPeakDbfs ?? null,
    aecOutputPeakDbfs: metrics.aecOutputPeakDbfs ?? null,
    outputPeakDbfs: metrics.outputPeakDbfs ?? null,
    hostDelayMs: metrics.hostDelayMs ?? null,
    rawSkewMs: metrics.hostDelayRawSkewMs ?? null,
    jitterMs: metrics.hostDelayJitterMs ?? null,
    calibrationState: metrics.hostDelayCalibrationState,
    aecEstimatedDelayMs: metrics.aecEstimatedDelayMs ?? null,
    erlDb: metrics.aecEchoReturnLossDb ?? null,
    erleDb: metrics.aecEchoReturnLossEnhancementDb ?? null,
    residualEchoLikelihood: metrics.aecResidualEchoLikelihood ?? null,
    aecAttenuationDb: metrics.aecAttenuationDb ?? null,
    vadProbability: metrics.vadProbability ?? null,
    speechVadActive: metrics.speechVadActive ?? null,
    vadGateOpen: metrics.vadGateOpen ?? null,
    echoGuardActive: metrics.echoGuardActive ?? null,
    renderDriftPpm: metrics.renderDriftPpm ?? null,
    captureDriftPpm: metrics.captureDriftPpm ?? null,
    renderQueueBlocks: metrics.renderQueueBlocks ?? 0,
    captureQueueBlocks: metrics.captureQueueBlocks ?? 0,
    processedBlocks: metrics.processedBlocks,
    renderUnderruns: metrics.renderUnderruns,
    deviceDiscontinuities: metrics.deviceDiscontinuities,
    hostDelayResetEvents: metrics.hostDelayResetEvents,
    deviceRestarts: metrics.deviceRestarts,
  };

  const previous = trace.samples[trace.samples.length - 1];
  const sameTelemetryWindow =
    previous != null &&
    (metrics.processedBlocks > 0
      ? previous.processedBlocks === metrics.processedBlocks
      : previous.elapsedSec === elapsedSec) &&
    previous.state === engine.state &&
    previous.output === engine.output &&
    previous.reasonCode === reasonCode &&
    previous.hostDelayResetEvents === metrics.hostDelayResetEvents &&
    previous.deviceDiscontinuities === metrics.deviceDiscontinuities &&
    previous.deviceRestarts === metrics.deviceRestarts;
  if (sameTelemetryWindow) {
    return trace;
  }
  const newEvents: DebugTimelineEvent[] = [];

  if (!previous) {
    newEvents.push({
      seq,
      elapsedSec,
      kind: "stateChange",
      summary: `trace started: state=${sample.state}, output=${sample.output}, reason=${sample.reasonCode ?? "none"}`,
    });
  } else {
    if (
      previous.state !== sample.state ||
      previous.output !== sample.output ||
      previous.reasonCode !== sample.reasonCode
    ) {
      newEvents.push({
        seq,
        elapsedSec,
        kind: "stateChange",
        summary: `state ${previous.state}/${previous.output}(${previous.reasonCode ?? "none"}) -> ${sample.state}/${sample.output}(${sample.reasonCode ?? "none"})`,
      });
    }
    if (sample.hostDelayResetEvents > previous.hostDelayResetEvents) {
      const delta = sample.hostDelayResetEvents - previous.hostDelayResetEvents;
      newEvents.push({
        seq,
        elapsedSec,
        kind: "delayReset",
        summary: `hostDelayReset +${delta} (rawSkew=${formatOptNumber(sample.rawSkewMs, "ms", 1)}, hostDelay=${formatOptNumber(sample.hostDelayMs, "ms", 0)}, aecDelay=${formatOptNumber(sample.aecEstimatedDelayMs, "ms", 0)})`,
      });
    }
    if (sample.deviceDiscontinuities > previous.deviceDiscontinuities) {
      const delta =
        sample.deviceDiscontinuities - previous.deviceDiscontinuities;
      newEvents.push({
        seq,
        elapsedSec,
        kind: "discontinuity",
        summary: `deviceDiscontinuity +${delta} (rawSkew=${formatOptNumber(sample.rawSkewMs, "ms", 1)})`,
      });
    }
    if (sample.deviceRestarts > previous.deviceRestarts) {
      const delta = sample.deviceRestarts - previous.deviceRestarts;
      newEvents.push({
        seq,
        elapsedSec,
        kind: "deviceRestart",
        summary: `deviceRestart +${delta}`,
      });
    }
  }

  return {
    enabled: true,
    startedAtMs,
    nextSeq: seq + 1,
    samples: [...trace.samples, sample].slice(-MAX_DEBUG_TIMELINE_SAMPLES),
    events: [...trace.events, ...newEvents].slice(-MAX_DEBUG_TIMELINE_EVENTS),
  };
}

export function evaluateDebugFindings(
  samples: readonly DebugTimelineSample[],
): DebugFindingCode[] {
  if (samples.length < 3) return [];
  const findings: DebugFindingCode[] = [];
  const activeSamples = samples.filter(
    (sample) => sample.state === "processing" || sample.state === "degraded",
  );
  const micActiveSamples = activeSamples.filter(
    (sample) => sample.inputPeakDbfs != null && sample.inputPeakDbfs > -50,
  );
  const renderHeardSamples = activeSamples.filter(
    (sample) => sample.renderPeakDbfs != null && sample.renderPeakDbfs > -80,
  );

  if (micActiveSamples.length >= 3 && renderHeardSamples.length === 0) {
    findings.push("noRenderSignal");
  }

  const bothActiveSamples = activeSamples.filter(
    (sample) =>
      sample.state === "processing" &&
      sample.renderPeakDbfs != null &&
      sample.renderPeakDbfs > -45 &&
      sample.inputPeakDbfs != null &&
      sample.inputPeakDbfs > -50 &&
      sample.aecOutputPeakDbfs != null,
  );
  if (bothActiveSamples.length >= 3) {
    const avgReductionDb =
      bothActiveSamples.reduce(
        (acc, sample) =>
          acc + ((sample.inputPeakDbfs ?? 0) - (sample.aecOutputPeakDbfs ?? 0)),
        0,
      ) / bothActiveSamples.length;
    const maxErle = Math.max(
      ...bothActiveSamples.map((sample) => sample.erleDb ?? 0),
    );
    if (avgReductionDb < 1.5 && maxErle < 2.0) {
      findings.push("aecLowAttenuation");
    }
  }

  const first = samples[0];
  const last = samples[samples.length - 1];
  if (last.hostDelayResetEvents > first.hostDelayResetEvents) {
    findings.push("delayResets");
  }
  if (last.deviceDiscontinuities > first.deviceDiscontinuities) {
    findings.push("discontinuities");
  }

  if (findings.length === 0 && activeSamples.length >= 3) {
    findings.push("healthy");
  }
  return findings;
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
  debugTrace?: DebugTraceState | null,
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
    `Signal & AEC3: hostDelay=${formatOptNumber(metrics.hostDelayMs, "ms", 0)}(${metrics.hostDelayCalibrationState}, resets=${metrics.hostDelayResetEvents}), aecDelay=${formatOptNumber(metrics.aecEstimatedDelayMs, "ms", 0)}, erl=${formatOptNumber(metrics.aecEchoReturnLossDb, "dB", 1)}, erle=${formatOptNumber(metrics.aecEchoReturnLossEnhancementDb, "dB", 1)}, residualEcho=${formatOptNumber(metrics.aecResidualEchoLikelihood, "", 2)}, echoGuard=${metrics.echoGuardActive == null ? "n/a" : metrics.echoGuardActive ? "active" : "idle"}(events=${metrics.echoGuardActiveEvents}), vadProb=${formatOptNumber(metrics.vadProbability, "", 2)}, renderPeak=${formatOptNumber(metrics.renderPeakDbfs, "dBFS", 1)}, inputPeak=${formatOptNumber(metrics.inputPeakDbfs, "dBFS", 1)}, aecPeak=${formatOptNumber(metrics.aecOutputPeakDbfs, "dBFS", 1)}, outputPeak=${formatOptNumber(metrics.outputPeakDbfs, "dBFS", 1)}, clipped=${metrics.inputClippedSamples}, limited=${metrics.limiterLimitedSamples}`,
  );

  if (
    metrics.renderSampleRateHz != null ||
    metrics.captureSampleRateHz != null
  ) {
    const renderFormat =
      metrics.renderSampleRateHz != null
        ? `${metrics.renderSampleRateHz}Hz/${metrics.renderChannels ?? "?"}ch(keepalive=${metrics.renderKeepaliveActive == null ? "n/a" : metrics.renderKeepaliveActive ? "on" : "off"}, drift=${formatOptNumber(metrics.renderDriftPpm, "ppm", 1)}, q=${metrics.renderQueueBlocks ?? 0}, timeouts=${metrics.renderWaitTimeouts ?? 0})`
        : "inactive";
    const captureFormat =
      metrics.captureSampleRateHz != null
        ? `${metrics.captureSampleRateHz}Hz/${metrics.captureChannels ?? "?"}ch(drift=${formatOptNumber(metrics.captureDriftPpm, "ppm", 1)}, q=${metrics.captureQueueBlocks ?? 0}, timeouts=${metrics.captureWaitTimeouts ?? 0})`
        : "inactive";
    lines.push(
      `Stream format & clock: render=${renderFormat}, capture=${captureFormat}`,
    );
  }

  if (debugTrace && (debugTrace.samples.length > 0 || debugTrace.events.length > 0)) {
    const findings = evaluateDebugFindings(debugTrace.samples);
    lines.push(
      "",
      `--- Debug Trace (${debugTrace.samples.length}s captured, ${debugTrace.events.length} events, findings=${findings.join(",") || "insufficientData"}) ---`,
    );
    if (debugTrace.events.length > 0) {
      lines.push("Events:");
      for (const event of debugTrace.events) {
        lines.push(`  [+${event.elapsedSec}s #${event.seq}] ${event.summary}`);
      }
    }
    if (debugTrace.samples.length > 0) {
      lines.push(
        "Timeline (t | state/out | render/in/aec/out dBFS | host/skew/aec ms | erl/erle/resEcho | vad/gate/guard | rQ/cQ):",
      );
      for (const sample of debugTrace.samples) {
        const stateTag = `${sample.state}/${sample.output}${sample.reasonCode ? `(${sample.reasonCode})` : ""}`;
        const levels = `${formatOptNumber(sample.renderPeakDbfs, "", 1)}/${formatOptNumber(sample.inputPeakDbfs, "", 1)}/${formatOptNumber(sample.aecOutputPeakDbfs, "", 1)}/${formatOptNumber(sample.outputPeakDbfs, "", 1)}`;
        const delays = `${formatOptNumber(sample.hostDelayMs, "", 0)}/${formatOptNumber(sample.rawSkewMs, "", 1)}/${formatOptNumber(sample.aecEstimatedDelayMs, "", 0)}`;
        const echo = `${formatOptNumber(sample.erlDb, "", 1)}/${formatOptNumber(sample.erleDb, "", 1)}/${formatOptNumber(sample.residualEchoLikelihood, "", 2)}`;
        const gate = `${formatOptNumber(sample.vadProbability, "", 2)}/${sample.vadGateOpen == null ? "-" : sample.vadGateOpen ? "open" : "closed"}/${sample.echoGuardActive == null ? "-" : sample.echoGuardActive ? "guard" : "idle"}`;
        lines.push(
          `  +${String(sample.elapsedSec).padStart(3, " ")}s | ${stateTag} | ${levels} | ${delays} | ${echo} | ${gate} | ${sample.renderQueueBlocks}/${sample.captureQueueBlocks}`,
        );
      }
    }
  }

  return lines.join("\n");
}
