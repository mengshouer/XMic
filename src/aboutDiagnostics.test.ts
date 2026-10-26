import { describe, expect, it } from "vitest";
import {
  type ApplicationDiagnostics,
  buildRedactedApplicationDiagnostics,
  createDebugTraceState,
  dspValidationState,
  type EngineDiagnosticSnapshot,
  evaluateDebugFindings,
  mergeLiveDspValidation,
  recordDebugSnapshot,
} from "./aboutDiagnostics";

const diagnostics: ApplicationDiagnostics = {
  mode: "portable",
  dataDirectory: "D:\\private\\XMic\\data",
  runtimeDirectory: "D:\\private\\XMic\\data\\runtime",
  webviewDataDirectory: "D:\\private\\XMic\\data\\webview",
  dspSource: "executableSidecar",
  dspCustom: true,
  dspLibraryPath: "D:\\private\\XMic\\xmic_webrtc_dsp.dll",
  dspLibraryFileName: "xmic_webrtc_dsp.dll",
  dspSelectionPinned: true,
  dspSelectionGuardError: null,
  expectedDspAbi: 2,
  actualDspAbi: 2,
  dspValidationError: null,
};

const engineFixture: EngineDiagnosticSnapshot = {
  desiredEnabled: true,
  state: "degraded",
  output: "raw",
  primaryReason: {
    code: "safetyConflict",
    detail: "Device {0.0.1.00000000}.{secret-guid} is in conflict",
    endpointId: "{0.0.1.00000000}.{secret-guid}",
    retryAfterMs: 1500,
    configurationIssue: {
      type: "virtualRouteConflict",
      kind: "defaultRender",
      endpointId: "{0.0.0.00000000}.{secret-render-guid}",
    },
  },
  settings: {
    followDefaultRender: true,
    renderEndpointId: "{0.0.0.00000000}.{secret-render-guid}",
    captureEndpointId: "{0.0.1.00000000}.{secret-mic-guid}",
    captureChannel: 0,
    outputEndpointId: "{0.0.0.00000000}.{secret-cable-guid}",
    virtualDeviceMode: "vbCable",
    degradedOutput: "raw",
    dsp: {
      aec: {
        enabled: true,
        preset: "balanced",
        highPassFilterEnabled: true,
      },
      rnnoise: {
        enabled: true,
        mix: 0.85,
      },
      agc2: {
        enabled: false,
        adaptiveDigitalEnabled: true,
        fixedGainDb: 0,
        headroomDb: 6,
        maxGainDb: 30,
      },
      vadGate: {
        enabled: true,
        threshold: 0.6,
        confirmationMs: 40,
        holdMs: 220,
        floorDb: -20,
      },
      limiter: {
        ceilingDbfs: -1,
        releaseMs: 50,
      },
    },
  },
  activeRoute: {
    renderEndpointId: "{0.0.0.00000000}.{secret-render-guid}",
    captureEndpointId: "{0.0.1.00000000}.{secret-mic-guid}",
    outputEndpointId: "{0.0.0.00000000}.{secret-cable-guid}",
  },
  metrics: {
    processedBlocks: 4200,
    renderUnderruns: 0,
    renderOverflows: 1,
    captureOverflows: 2,
    deviceDiscontinuities: 3,
    outputUnderruns: 0,
    outputQueueOverflows: 0,
    outputQueueDepthMs: 12.5,
    outputRateAdjustmentPpm: -15,
    deviceRestarts: 1,
    dspFailures: 0,
    processingTimeP99Us: 380,
    processingTimeMaxUs: 920,
    processingDeadlineMisses: 0,
    processingBudgetExceededBlocks: 0,
    processingOverloadEvents: 0,
    processingRecoveryEvents: 1,
    processingLoadMode: "full",
    captureLatencyMs: 12.0,
    dspLatencyMs: 80.0,
    outputLatencyMs: 10.5,
    pipelineLatencyMs: 102.5,
    hostDelayMs: 24,
    hostDelayCalibrationState: "calibrated",
    hostDelayResetEvents: 0,
    aecEstimatedDelayMs: 28,
    aecEchoReturnLossDb: 14.2,
    aecEchoReturnLossEnhancementDb: 19.5,
    aecResidualEchoLikelihood: 0.03,
    inputPeakDbfs: -12.4,
    outputPeakDbfs: -13.1,
    inputClippedSamples: 0,
    limiterLimitedSamples: 4,
    vadProbability: 0.94,
    echoGuardActive: false,
    echoGuardActiveEvents: 2,
  },
};

describe("buildRedactedApplicationDiagnostics", () => {
  it("keeps support metadata while omitting every full path", () => {
    const text = buildRedactedApplicationDiagnostics(diagnostics, "0.3.5");

    expect(text).toContain("XMic version: 0.3.5");
    expect(text).toContain("Mode: portable");
    expect(text).toContain("DSP source: executableSidecar");
    expect(text).toContain("DSP library file: xmic_webrtc_dsp.dll");
    expect(text).toContain("Expected DSP ABI: 2");
    expect(text).not.toContain("D:\\private");
    expect(text).not.toContain(diagnostics.dataDirectory);
    expect(text).not.toContain(diagnostics.runtimeDirectory);
    expect(text).not.toContain(diagnostics.webviewDataDirectory);
    expect(text).not.toContain(diagnostics.dspLibraryPath);
  });

  it("redacts selection-guard details and reports validation failure", () => {
    const text = buildRedactedApplicationDiagnostics(
      {
        ...diagnostics,
        dspSelectionGuardError:
          "D:\\Users\\private\\custom.dll changed during this run",
      },
      "0.3.5",
    );

    expect(text).toContain("DSP validation: failed");
    expect(text).not.toContain("Users");
    expect(text).not.toContain("changed during this run");
  });

  it("does not copy a validation error that may itself contain a path", () => {
    const text = buildRedactedApplicationDiagnostics(
      {
        ...diagnostics,
        actualDspAbi: null,
        dspValidationError:
          "load D:\\Users\\private\\custom.dll failed with access denied",
      },
      null,
    );

    expect(text).toContain("Actual DSP ABI: unavailable");
    expect(text).toContain("DSP validation: failed");
    expect(text).not.toContain("Users");
    expect(text).not.toContain("access denied");
  });

  it("reports pending while the helper has not answered, never valid", () => {
    const unanswered: ApplicationDiagnostics = {
      ...diagnostics,
      actualDspAbi: null,
      dspSelectionPinned: false,
      dspValidationError: null,
    };

    const text = buildRedactedApplicationDiagnostics(unanswered, "0.3.5");

    expect(dspValidationState(unanswered)).toBe("pending");
    expect(text).toContain("Actual DSP ABI: unavailable");
    expect(text).toContain("DSP validation: pending");
    expect(text).not.toContain("DSP validation: valid");
  });

  it("treats a mismatching ABI readback as failed even without a message", () => {
    const mismatch: ApplicationDiagnostics = {
      ...diagnostics,
      actualDspAbi: diagnostics.expectedDspAbi + 1,
      dspValidationError: null,
    };

    expect(dspValidationState(mismatch)).toBe("failed");
    expect(buildRedactedApplicationDiagnostics(mismatch, "0.3.5")).toContain(
      "DSP validation: failed",
    );
  });

  it("includes engine state, route summary, DSP chain, and metrics while redacting endpoint IDs and details", () => {
    const text = buildRedactedApplicationDiagnostics(
      diagnostics,
      "0.4.0",
      engineFixture,
    );

    expect(text).toContain("Engine state: degraded (desired=on, output=raw)");
    expect(text).toContain(
      "Primary reason: safetyConflict, issue=virtualRouteConflict(defaultRender), retryAfterMs=1500",
    );
    expect(text).toContain(
      "Route config: render=followDefault, capture=explicit(ch0), virtual=vbCable, degradedOutput=raw",
    );
    expect(text).toContain("Active route: render=bound, capture=bound, output=bound");
    expect(text).toContain(
      "DSP chain: AEC3=on(balanced, hpf=on) | RNNoise=on(mix=0.85) | AGC2=off | VAD=on(thr=0.60, confirm=40ms, hold=220ms, floor=-20dB) | Limiter=-1dBFS/50ms",
    );
    expect(text).toContain(
      "Latency: total=102.5ms (capture=12.0ms, dsp=80.0ms, output=10.5ms), queueDepth=12.5ms, rateAdjust=-15ppm",
    );
    expect(text).toContain(
      "Stream health: blocks=4200, captureOverflows=2, renderOverflows=1, renderUnderruns=0, outputUnderruns=0, outputQueueOverflows=0, discontinuities=3, restarts=1, dspFailures=0",
    );
    expect(text).not.toContain("secret-guid");
    expect(text).not.toContain("secret-render-guid");
    expect(text).not.toContain("secret-mic-guid");
    expect(text).not.toContain("secret-cable-guid");
    expect(text).not.toContain("is in conflict");
  });

  it("refreshes stale pending DSP validation from live engine status while redacting error details", () => {
    const stalePending: ApplicationDiagnostics = {
      ...diagnostics,
      actualDspAbi: null,
      dspValidationError: null,
    };
    const liveValidEngine: EngineDiagnosticSnapshot = {
      ...engineFixture,
      dspValidation: {
        expectedAbi: 2,
        actualAbi: 2,
        error: null,
      },
    };

    const merged = mergeLiveDspValidation(stalePending, liveValidEngine);
    expect(merged.actualDspAbi).toBe(2);
    expect(dspValidationState(merged)).toBe("valid");

    // A transient pending engine snapshot must not overwrite an already-resolved validation result.
    const transientPendingEngine: EngineDiagnosticSnapshot = {
      ...engineFixture,
      dspValidation: {
        expectedAbi: 2,
        actualAbi: null,
        error: null,
      },
    };
    expect(
      dspValidationState(
        mergeLiveDspValidation(diagnostics, transientPendingEngine),
      ),
    ).toBe("valid");

    const liveFailedEngine: EngineDiagnosticSnapshot = {
      ...engineFixture,
      dspValidation: {
        expectedAbi: 2,
        actualAbi: null,
        error: "load C:\\Users\\private\\broken.dll failed",
      },
    };
    const text = buildRedactedApplicationDiagnostics(
      stalePending,
      "0.4.0",
      liveFailedEngine,
    );
    expect(text).toContain("Actual DSP ABI: unavailable");
    expect(text).toContain("DSP validation: failed");
    expect(text).not.toContain("broken.dll");

    const boostedEngine: EngineDiagnosticSnapshot = {
      ...engineFixture,
      settings: {
        ...engineFixture.settings,
        dsp: {
          ...engineFixture.settings.dsp,
          limiter: {
            ...engineFixture.settings.dsp.limiter,
            gainDb: 10,
          },
        },
      },
    };
    expect(
      buildRedactedApplicationDiagnostics(diagnostics, "0.4.0", boostedEngine),
    ).toContain("Limiter=gain=+10dB,-1dBFS/50ms");
  });

  it("records debug timeline samples, flags missing render signal, and appends redacted trace", () => {
    let trace = createDebugTraceState(true);
    const silentRenderEngine: EngineDiagnosticSnapshot = {
      ...engineFixture,
      state: "processing",
      output: "processed",
      primaryReason: null,
      metrics: {
        ...engineFixture.metrics,
        renderPeakDbfs: -120,
        inputPeakDbfs: -22,
        aecOutputPeakDbfs: -22,
        outputPeakDbfs: -22,
        renderSampleRateHz: 48000,
        renderChannels: 2,
        captureSampleRateHz: 48000,
        captureChannels: 1,
        renderKeepaliveActive: true,
        renderDriftPpm: -1.5,
        captureDriftPpm: 2.0,
      },
    };

    trace = recordDebugSnapshot(trace, silentRenderEngine, 1000);
    trace = recordDebugSnapshot(
      trace,
      {
        ...silentRenderEngine,
        metrics: { ...silentRenderEngine.metrics, processedBlocks: 4300 },
      },
      2000,
    );
    trace = recordDebugSnapshot(
      trace,
      {
        ...silentRenderEngine,
        metrics: { ...silentRenderEngine.metrics, processedBlocks: 4400 },
      },
      3000,
    );

    expect(evaluateDebugFindings(trace.samples)).toContain("noRenderSignal");

    const text = buildRedactedApplicationDiagnostics(
      diagnostics,
      "0.4.1",
      silentRenderEngine,
      trace,
    );
    expect(text).toContain(
      "Stream format & clock: render=48000Hz/2ch(keepalive=on, drift=-1.5ppm, q=0, timeouts=0), capture=48000Hz/1ch(drift=2.0ppm, q=0, timeouts=0)",
    );
    expect(text).toContain("--- Debug Trace (3s captured");
    expect(text).toContain("findings=noRenderSignal");
  });
});
