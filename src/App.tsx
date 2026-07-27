import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import {
  Activity,
  AlertTriangle,
  ArrowRight,
  AudioLines,
  AudioWaveform,
  Cable,
  ChevronRight,
  CircleCheck,
  CircleGauge,
  Clock3,
  Copy,
  Cpu,
  ExternalLink,
  Gauge,
  GitMerge,
  Languages,
  LayoutDashboard,
  LoaderCircle,
  LockKeyhole,
  Mic2,
  Network,
  Play,
  Power,
  Radio,
  RefreshCw,
  Route,
  Save,
  ShieldCheck,
  SlidersHorizontal,
  Speaker,
  Square,
  Trash2,
  Waves,
} from "lucide-react";
import {
  type ReactNode,
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
} from "react";
import "./App.css";
import {
  ControlLabel,
  NumberControl,
  RangeControl,
  SelectControl,
  Toggle,
} from "./components/Controls";
import { TelemetryCanvas } from "./components/TelemetryCanvas";
import {
  readStoredLanguage,
  storeLanguage,
  translations,
  type Language,
  type Translation,
} from "./i18n";

type DataFlow = "render" | "capture";
type DeviceRole = "console" | "multimedia" | "communications";
type EngineMode =
  | "sleeping"
  | "starting"
  | "converging"
  | "active"
  | "rawFallback"
  | "safeMode"
  | "muted"
  | "error";
type AecPreset = "natural" | "balanced" | "strong";
type DspFailurePolicy = "rawPassthrough" | "mute";
type VirtualDeviceMode = "vbCable" | "custom";
type VirtualDeviceIssueReason =
  | "vbCableUnavailable"
  | "customPairIncomplete"
  | "customPairUnconfirmed";
type VirtualRouteConflictKind =
  | "defaultRender"
  | "fixedRender"
  | "physicalCapture";
type DelayCalibrationState = "uncalibrated" | "warmingUp" | "calibrated";
type ProcessingLoadMode = "full" | "reduced" | "protection";
type ViewId = "monitor" | "routing" | "processing";
type DspStageId = "aec" | "rnnoise" | "agc2" | "vad" | "limiter";
type DspFeatureId = "aec" | "rnnoise" | "agc2" | "vadGate";

interface PhysicalCaptureRequiredIssue {
  type: "physicalCaptureRequired";
  virtualCaptureEndpointId: string | null;
}

interface VirtualDeviceRequiredIssue {
  type: "virtualDeviceRequired";
  reason: VirtualDeviceIssueReason;
}

interface VirtualRouteConflictIssue {
  type: "virtualRouteConflict";
  kind: VirtualRouteConflictKind;
  endpointId: string;
}

type ConfigurationIssue =
  | PhysicalCaptureRequiredIssue
  | VirtualDeviceRequiredIssue
  | VirtualRouteConflictIssue;

interface EndpointDescriptor {
  platform: string;
  id: string;
  groupId?: string | null;
  friendlyName: string;
  flow: DataFlow;
  defaultRoles: DeviceRole[];
}

interface AecSettings {
  enabled: boolean;
  preset: AecPreset;
  highPassFilterEnabled: boolean;
}

interface RnnoiseSettings {
  enabled: boolean;
  mix: number;
}

interface Agc2Settings {
  enabled: boolean;
  adaptiveDigitalEnabled: boolean;
  fixedGainDb: number;
  headroomDb: number;
  maxGainDb: number;
  initialGainDb: number;
  maxGainChangeDbPerSecond: number;
  maxOutputNoiseLevelDbfs: number;
}

interface VadGateSettings {
  enabled: boolean;
  threshold: number;
  confirmationMs: number;
  holdMs: number;
  floorDb: number;
  attackMs: number;
  releaseMs: number;
}

interface LimiterSettings {
  ceilingDbfs: number;
  releaseMs: number;
}

interface DspSettings {
  aec: AecSettings;
  rnnoise: RnnoiseSettings;
  agc2: Agc2Settings;
  vadGate: VadGateSettings;
  limiter: LimiterSettings;
}

interface TrustedVirtualAudioPair {
  renderEndpointId: string;
  captureEndpointId: string;
  renderName: string;
  captureName: string;
}

interface EngineSettings {
  followDefaultRender: boolean;
  renderEndpointId: string | null;
  captureEndpointId: string | null;
  captureChannel: number | null;
  outputEndpointId: string | null;
  virtualDeviceMode: VirtualDeviceMode | null;
  virtualCaptureEndpointId: string | null;
  trustedVirtualAudioPairs: TrustedVirtualAudioPair[];
  failurePolicy: DspFailurePolicy;
  dsp: DspSettings;
}

interface ActiveRoute {
  renderEndpointId: string;
  captureEndpointId: string;
  outputEndpointId: string;
}

interface ActiveDeviceProfile {
  renderEndpointId: string;
  captureEndpointId: string;
  customized: boolean;
}

interface EngineMetrics {
  processedBlocks: number;
  renderUnderruns: number;
  renderOverflows: number;
  captureOverflows: number;
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
  processingLoadMode: ProcessingLoadMode;
  captureLatencyMs: number;
  dspLatencyMs: number;
  outputLatencyMs: number;
  pipelineLatencyMs: number;
  hostDelayMs?: number | null;
  hostDelayRawSkewMs?: number | null;
  hostDelayJitterMs?: number | null;
  hostDelayCalibrationState: DelayCalibrationState;
  hostDelayResetEvents: number;
  aecEstimatedDelayMs?: number | null;
  aecEchoReturnLossDb?: number | null;
  aecEchoReturnLossEnhancementDb?: number | null;
  aecResidualEchoLikelihood?: number | null;
  aecAttenuationDb?: number | null;
  inputPeakDbfs?: number | null;
  outputPeakDbfs?: number | null;
  inputClippedSamples?: number;
  limiterLimitedSamples?: number;
  vadProbability?: number | null;
  speechVadActive?: boolean | null;
  vadGateOpen?: boolean | null;
  vadGateOpenEvents?: number;
  echoGuardActive?: boolean | null;
  echoGuardActiveEvents?: number;
  echoGuardNearendCandidateEvents?: number;
  echoGuardReleaseEvents?: number;
}

interface EngineStatus {
  protocolVersion: number;
  running: boolean;
  mode: EngineMode;
  detail: string;
  endpoints: EndpointDescriptor[];
  settings: EngineSettings;
  activeRoute: ActiveRoute | null;
  activeDeviceProfile: ActiveDeviceProfile | null;
  configurationIssue: ConfigurationIssue | null;
  metrics: EngineMetrics;
  warning: string | null;
}

interface LifecyclePreferences {
  launchAtLogin: boolean;
  startInTray: boolean;
  startEngineOnLaunch: boolean;
}

interface LifecycleStatus extends LifecyclePreferences {
  sessionAvailable: boolean;
  resumePending: boolean;
  lastError: string | null;
}

function formatOptionalMetric(
  value: number | null | undefined,
  suffix: string,
  digits = 0,
) {
  return value == null ? "--" : `${value.toFixed(digits)} ${suffix}`;
}

// Mirrors processing_latency_frames() in crates/engine-runtime/src/processor.rs so the editor can
// price a gate change before it is applied. Only the DSP segment is predictable from settings alone;
// the capture and output segments are measured at runtime and reported in EngineMetrics.
const FRAME_DURATION_MS = 10;
const GATE_SPEECH_ONSET_PRE_ROLL_FRAMES = 4;
const ECHO_GUARD_NEAREND_CONFIRMATION_FRAMES = 6;
const ECHO_GUARD_SMOOTHING_PRE_ROLL_FRAMES = 2;

function estimatedDspLatencyMs(dsp: DspSettings) {
  const confirmationFrames = Math.ceil(dsp.vadGate.confirmationMs / FRAME_DURATION_MS);
  const echoGuardEnabled =
    dsp.aec.enabled && dsp.vadGate.enabled && dsp.aec.preset === "strong";
  const gateFrames = dsp.vadGate.enabled
    ? Math.max(confirmationFrames - 1, 0) +
      GATE_SPEECH_ONSET_PRE_ROLL_FRAMES +
      (echoGuardEnabled
        ? ECHO_GUARD_NEAREND_CONFIRMATION_FRAMES -
          1 +
          ECHO_GUARD_SMOOTHING_PRE_ROLL_FRAMES
        : 0)
    : 0;
  return ((dsp.rnnoise.enabled ? 1 : 0) + gateFrames) * FRAME_DURATION_MS;
}

function App() {
  const [language, setLanguage] = useState<Language>(readStoredLanguage);
  const [activeView, setActiveView] = useState<ViewId>("monitor");
  const [activeDspStage, setActiveDspStage] =
    useState<DspStageId>("aec");
  const [status, setStatus] = useState<EngineStatus | null>(null);
  const [lifecycleStatus, setLifecycleStatus] =
    useState<LifecycleStatus | null>(null);
  const [draft, setDraft] = useState<EngineSettings | null>(null);
  const [dirty, setDirty] = useState(false);
  const [busy, setBusy] = useState(false);
  const [quickDspBusy, setQuickDspBusy] = useState<DspFeatureId | null>(null);
  const [lifecycleBusy, setLifecycleBusy] = useState(false);
  const [pageVisible, setPageVisible] = useState(!document.hidden);
  const [error, setError] = useState<string | null>(null);
  const [diagnosticsCopied, setDiagnosticsCopied] = useState(false);
  const captureSelectRef = useRef<HTMLSelectElement>(null);
  const virtualOutputSelectRef = useRef<HTMLSelectElement>(null);
  const copy = translations[language];

  const navigateToView = useCallback((view: ViewId) => {
    setActiveView(view);
    const container = document.querySelector<HTMLElement>(".page-content");
    const section = document.getElementById(`view-${view}`);
    if (!container || !section) return;
    const target =
      section.querySelector<HTMLElement>(".view-section-heading") ?? section;
    const top =
      container.scrollTop +
      target.getBoundingClientRect().top -
      container.getBoundingClientRect().top -
      14;
    container.scrollTo({
      top,
      behavior: window.matchMedia("(prefers-reduced-motion: reduce)").matches
        ? "auto"
        : "smooth",
    });
  }, []);

  const focusPhysicalCapture = useCallback(() => {
    navigateToView("routing");
    window.setTimeout(() => captureSelectRef.current?.focus(), 180);
  }, [navigateToView]);

  const focusVirtualDevice = useCallback(() => {
    navigateToView("routing");
    window.setTimeout(() => virtualOutputSelectRef.current?.focus(), 180);
  }, [navigateToView]);

  const focusPlaybackReference = useCallback(() => {
    navigateToView("routing");
    window.setTimeout(() => {
      document
        .getElementById("playback-reference-configuration")
        ?.querySelector<HTMLElement>(
          "input:not(:disabled), select:not(:disabled), button:not(:disabled)",
        )
        ?.focus();
    }, 180);
  }, [navigateToView]);

  const focusConfigurationIssue = useCallback(
    (issue: ConfigurationIssue) => {
      if (issue.type === "physicalCaptureRequired") {
        focusPhysicalCapture();
      } else if (issue.type === "virtualRouteConflict") {
        focusPlaybackReference();
      } else {
        focusVirtualDevice();
      }
    },
    [focusPhysicalCapture, focusPlaybackReference, focusVirtualDevice],
  );

  useEffect(() => {
    document.documentElement.lang = language;
    storeLanguage(language);
    invoke("set_ui_language", { language }).catch((reason) =>
      setError(String(reason)),
    );
  }, [language]);

  const acceptStatus = useCallback(
    (next: EngineStatus, forceSettings = false) => {
      setStatus(next);
      if (forceSettings || !dirty) setDraft(next.settings);
    },
    [dirty],
  );

  const refresh = useCallback(async () => {
    try {
      const next = await invoke<EngineStatus>("get_engine_status");
      acceptStatus(next);
      setError(null);
    } catch (reason) {
      setError(String(reason));
    }
  }, [acceptStatus]);

  useEffect(() => {
    refresh();
  }, [refresh]);

  useEffect(() => {
    invoke<LifecycleStatus>("get_lifecycle_status")
      .then(setLifecycleStatus)
      .catch((reason) => setError(String(reason)));
  }, []);

  useEffect(() => {
    let disposed = false;
    let unlisten: (() => void) | undefined;
    listen<ConfigurationIssue>("navigate-to-configuration", (event) => {
      if (!disposed) focusConfigurationIssue(event.payload);
    }).then((stopListening) => {
      if (disposed) stopListening();
      else unlisten = stopListening;
    });
    return () => {
      disposed = true;
      unlisten?.();
    };
  }, [focusConfigurationIssue]);

  useEffect(() => {
    let disposed = false;
    let unlisten: (() => void) | undefined;
    listen<EngineStatus>("engine-status", (event) => {
      if (!disposed) acceptStatus(event.payload);
    }).then((stopListening) => {
      if (disposed) stopListening();
      else unlisten = stopListening;
    });
    return () => {
      disposed = true;
      unlisten?.();
    };
  }, [acceptStatus]);

  useEffect(() => {
    let disposed = false;
    let unlisten: (() => void) | undefined;
    listen<LifecycleStatus>("lifecycle-status", (event) => {
      if (!disposed) setLifecycleStatus(event.payload);
    }).then((stopListening) => {
      if (disposed) stopListening();
      else unlisten = stopListening;
    });
    return () => {
      disposed = true;
      unlisten?.();
    };
  }, []);

  useEffect(() => {
    const updateVisibility = () => {
      const visible = !document.hidden;
      setPageVisible(visible);
      if (visible) refresh();
    };
    document.addEventListener("visibilitychange", updateVisibility);
    return () => document.removeEventListener("visibilitychange", updateVisibility);
  }, [refresh]);

  useEffect(() => {
    if (!status?.running || !pageVisible) return;
    const timer = window.setInterval(refresh, 1000);
    return () => window.clearInterval(timer);
  }, [pageVisible, refresh, status?.running]);

  const renderEndpoints = useMemo(
    () => status?.endpoints.filter((endpoint) => endpoint.flow === "render") ?? [],
    [status],
  );
  const allCaptureEndpoints = useMemo(
    () => status?.endpoints.filter((endpoint) => endpoint.flow === "capture") ?? [],
    [status],
  );
  const vbCablePair = useMemo(
    () => findStandardVbCablePair(status?.endpoints ?? []),
    [status],
  );
  const effectiveVirtualMode: VirtualDeviceMode =
    draft?.virtualDeviceMode === "custom" || !vbCablePair
      ? "custom"
      : "vbCable";
  const captureEndpoints = useMemo(
    () =>
      status?.endpoints.filter(
        (endpoint) =>
          endpoint.flow === "capture" &&
          !isConfiguredVirtualCapture(endpoint, draft, status.endpoints) &&
          !sameEndpointGroup(
            endpoint,
            status.endpoints.find(
              (candidate) => candidate.id === draft?.outputEndpointId,
            ),
          ),
      ) ?? [],
    [draft, status],
  );
  const otherOutputs = useMemo(
    () =>
      renderEndpoints.filter(
        (endpoint) => endpoint.id !== vbCablePair?.render.id,
      ),
    [renderEndpoints, vbCablePair?.render.id],
  );
  const customCaptureEndpoints = useMemo(
    () =>
      allCaptureEndpoints.filter(
        (endpoint) =>
          endpoint.id !== draft?.captureEndpointId &&
          endpoint.id !== vbCablePair?.capture.id,
      ),
    [allCaptureEndpoints, draft?.captureEndpointId, vbCablePair?.capture.id],
  );
  const customPairTrusted = useMemo(
    () => (draft ? activeCustomPairIsTrusted(draft) : false),
    [draft],
  );
  const customPairOffline = useMemo(
    () =>
      Boolean(
        draft?.virtualDeviceMode === "custom" &&
          customPairTrusted &&
          (!renderEndpoints.some(
            (endpoint) => endpoint.id === draft.outputEndpointId,
          ) ||
            !allCaptureEndpoints.some(
              (endpoint) => endpoint.id === draft.virtualCaptureEndpointId,
            )),
      ),
    [allCaptureEndpoints, customPairTrusted, draft, renderEndpoints],
  );
  const configurationIssue = useMemo(
    () => {
      if (!draft || !status) return null;
      return (
        getConfigurationIssue(draft, status.endpoints) ??
        (dirty ? null : status.configurationIssue)
      );
    },
    [dirty, draft, status],
  );
  const captureConfigurationIssue =
    configurationIssue?.type === "physicalCaptureRequired"
      ? configurationIssue
      : null;
  const virtualConfigurationIssue =
    configurationIssue?.type === "virtualDeviceRequired"
      ? configurationIssue
      : null;
  const configurationIssueMessage = useMemo(
    () =>
      configurationIssue
        ? describeConfigurationIssue(
            configurationIssue,
            status?.endpoints ?? [],
            copy.routing,
          )
        : null,
    [configurationIssue, copy.routing, status?.endpoints],
  );
  const followDefaultCaptureBlocked = useMemo(
    () =>
      draft && status
        ? effectiveVirtualMode === "custom" ||
          getConfigurationIssue(
            { ...draft, captureEndpointId: null },
            status.endpoints,
          )?.type === "physicalCaptureRequired"
        : false,
    [draft, effectiveVirtualMode, status],
  );
  const running = status?.running ?? false;
  const engineRequested = running || (lifecycleStatus?.resumePending ?? false);
  const controlsDisabled = running || busy || quickDspBusy !== null;

  const changeSettings = (update: (current: EngineSettings) => EngineSettings) => {
    setDraft((current) => (current ? update(current) : current));
    setDirty(true);
  };

  const selectVirtualMode = (mode: VirtualDeviceMode) => {
    changeSettings((current) => {
      if (mode === "vbCable" && vbCablePair) {
        return {
          ...current,
          virtualDeviceMode: "vbCable",
          outputEndpointId: vbCablePair.render.id,
          virtualCaptureEndpointId: vbCablePair.capture.id,
        };
      }
      return {
        ...current,
        virtualDeviceMode: "custom",
        outputEndpointId:
          current.virtualDeviceMode === "custom"
            ? current.outputEndpointId
            : null,
        virtualCaptureEndpointId:
          current.virtualDeviceMode === "custom"
            ? current.virtualCaptureEndpointId
            : null,
      };
    });
  };

  const confirmCustomPair = () => {
    if (!draft?.outputEndpointId || !draft.virtualCaptureEndpointId) return;
    const render = renderEndpoints.find(
      (endpoint) => endpoint.id === draft.outputEndpointId,
    );
    const capture = allCaptureEndpoints.find(
      (endpoint) => endpoint.id === draft.virtualCaptureEndpointId,
    );
    if (!render || !capture) return;
    const trusted: TrustedVirtualAudioPair = {
      renderEndpointId: render.id,
      captureEndpointId: capture.id,
      renderName: render.friendlyName,
      captureName: capture.friendlyName,
    };
    changeSettings((current) => ({
      ...current,
      trustedVirtualAudioPairs: [
        ...current.trustedVirtualAudioPairs.filter(
          (pair) =>
            pair.renderEndpointId !== trusted.renderEndpointId ||
            pair.captureEndpointId !== trusted.captureEndpointId,
        ),
        trusted,
      ].slice(-32),
    }));
  };

  const revokeTrustedPair = async (pair: TrustedVirtualAudioPair) => {
    if (!draft) return;
    const settings = {
      ...draft,
      trustedVirtualAudioPairs: draft.trustedVirtualAudioPairs.filter(
        (candidate) =>
          candidate.renderEndpointId !== pair.renderEndpointId ||
          candidate.captureEndpointId !== pair.captureEndpointId,
      ),
    };
    setBusy(true);
    setError(null);
    try {
      if (engineRequested) await invoke<EngineStatus>("stop_engine");
      const next = await invoke<EngineStatus>("update_engine_settings", {
        settings,
      });
      setDirty(false);
      acceptStatus(next, true);
    } catch (reason) {
      setError(String(reason));
    } finally {
      setBusy(false);
    }
  };

  const copyDeviceDiagnostics = async () => {
    if (!status) return;
    const text = buildDeviceDiagnostics(
      status.endpoints,
      draft ?? status.settings,
      configurationIssue,
    );
    try {
      await navigator.clipboard.writeText(text);
      setDiagnosticsCopied(true);
      window.setTimeout(() => setDiagnosticsCopied(false), 1800);
    } catch (reason) {
      setError(String(reason));
    }
  };

  const openVbCableDownload = async () => {
    try {
      await invoke("open_vb_cable_download");
    } catch (reason) {
      setError(String(reason));
    }
  };

  const applySettings = async (): Promise<boolean> => {
    if (!draft) return false;
    setBusy(true);
    setError(null);
    try {
      const next = await invoke<EngineStatus>("update_engine_settings", {
        settings: draft,
      });
      setDirty(false);
      acceptStatus(next, true);
      return true;
    } catch (reason) {
      setError(String(reason));
      return false;
    } finally {
      setBusy(false);
    }
  };

  const mergeQuickDspStatus = (
    next: EngineStatus,
    feature: DspFeatureId,
  ) => {
    acceptStatus(next);
    const enabled = dspFeatureEnabled(next.settings, feature);
    setDraft((current) =>
      current ? withDspFeatureEnabled(current, feature, enabled) : next.settings,
    );
  };

  const setQuickDspFeature = async (
    feature: DspFeatureId,
    enabled: boolean,
  ) => {
    if (quickDspBusy) return;
    setQuickDspBusy(feature);
    setError(null);
    try {
      const next = await invoke<EngineStatus>("set_dsp_feature_enabled", {
        feature,
        enabled,
      });
      mergeQuickDspStatus(next, feature);
    } catch (reason) {
      setError(String(reason));
      try {
        const next = await invoke<EngineStatus>("get_engine_status");
        mergeQuickDspStatus(next, feature);
      } catch {
        // Keep the original reconfiguration error visible.
      }
    } finally {
      setQuickDspBusy(null);
    }
  };

  const toggleEngine = async () => {
    if (!engineRequested && configurationIssue) {
      focusConfigurationIssue(configurationIssue);
      return;
    }
    setBusy(true);
    setError(null);
    try {
      if (engineRequested) {
        const next = await invoke<EngineStatus>("stop_engine");
        acceptStatus(next, true);
      } else {
        if (dirty && draft) {
          const next = await invoke<EngineStatus>("update_engine_settings", {
            settings: draft,
          });
          setDirty(false);
          acceptStatus(next, true);
        }
        const next = await invoke<EngineStatus>("start_engine");
        acceptStatus(next, true);
      }
    } catch (reason) {
      setError(String(reason));
    } finally {
      setBusy(false);
    }
  };

  const retryEngine = async () => {
    setBusy(true);
    setError(null);
    try {
      const next = await invoke<EngineStatus>("start_engine");
      acceptStatus(next);
    } catch (reason) {
      setError(String(reason));
    } finally {
      setBusy(false);
    }
  };

  const updateLifecyclePreferences = async (
    patch: Partial<LifecyclePreferences>,
  ) => {
    if (!lifecycleStatus) return;
    const preferences: LifecyclePreferences = {
      launchAtLogin: lifecycleStatus.launchAtLogin,
      startInTray: lifecycleStatus.startInTray,
      startEngineOnLaunch: lifecycleStatus.startEngineOnLaunch,
      ...patch,
    };
    setLifecycleBusy(true);
    setError(null);
    try {
      const next = await invoke<LifecycleStatus>(
        "update_lifecycle_preferences",
        { preferences },
      );
      setLifecycleStatus(next);
    } catch (reason) {
      setError(String(reason));
    } finally {
      setLifecycleBusy(false);
    }
  };

  if (!status || !draft) {
    return (
      <div className="loading-screen">
        <span className="loading-mark">
          <AudioWaveform size={26} aria-hidden="true" />
        </span>
        <span>{copy.common.loading}</span>
      </div>
    );
  }

  const updateAec = (patch: Partial<AecSettings>) =>
    changeSettings((current) => ({
      ...current,
      dsp: { ...current.dsp, aec: { ...current.dsp.aec, ...patch } },
    }));
  const updateRnnoise = (patch: Partial<RnnoiseSettings>) =>
    changeSettings((current) => ({
      ...current,
      dsp: {
        ...current.dsp,
        rnnoise: { ...current.dsp.rnnoise, ...patch },
      },
    }));
  const updateAgc2 = (patch: Partial<Agc2Settings>) =>
    changeSettings((current) => ({
      ...current,
      dsp: { ...current.dsp, agc2: { ...current.dsp.agc2, ...patch } },
    }));
  // AGC2 without adaptive digital and without fixed gain only runs its internal limiter, which
  // validation rejects, so enabling the stage arms the adaptive default alongside it.
  const enableAgc2 = (enabled: boolean) =>
    updateAgc2(
      enabled &&
        !draft.dsp.agc2.adaptiveDigitalEnabled &&
        draft.dsp.agc2.fixedGainDb <= 0
        ? { enabled, adaptiveDigitalEnabled: true }
        : { enabled },
    );
  const updateVad = (patch: Partial<VadGateSettings>) =>
    changeSettings((current) => ({
      ...current,
      dsp: {
        ...current.dsp,
        vadGate: { ...current.dsp.vadGate, ...patch },
      },
    }));
  const updateLimiter = (patch: Partial<LimiterSettings>) =>
    changeSettings((current) => ({
      ...current,
      dsp: {
        ...current.dsp,
        limiter: { ...current.dsp.limiter, ...patch },
      },
    }));

  const defaultRender =
    renderEndpoints.find((endpoint) =>
      endpoint.defaultRoles.includes("multimedia"),
    ) ?? renderEndpoints[0];
  const defaultCapture =
    allCaptureEndpoints.find((endpoint) =>
      endpoint.defaultRoles.includes("multimedia"),
    );
  const selectedCapture = draft.captureEndpointId
    ? captureEndpoints.find(
        (endpoint) => endpoint.id === draft.captureEndpointId,
      )
    : followDefaultCaptureBlocked
      ? undefined
      : defaultCapture;
  const visibleRenderId =
    status.activeRoute?.renderEndpointId ??
    (draft.followDefaultRender
      ? defaultRender?.id
      : (draft.renderEndpointId ?? undefined));
  const visibleCaptureId =
    status.activeRoute?.captureEndpointId ??
    draft.captureEndpointId ??
    defaultCapture?.id;
  const visibleOutputId =
    status.activeRoute?.outputEndpointId ?? draft.outputEndpointId ?? undefined;
  const profileLabel = status.activeDeviceProfile
    ? status.activeDeviceProfile.customized
      ? copy.routing.customizedProfile
      : copy.routing.defaultProfile
    : copy.routing.waitingProfile;
  const stageEnabled: Record<DspStageId, boolean> = {
    aec: draft.dsp.aec.enabled,
    rnnoise: draft.dsp.rnnoise.enabled,
    agc2: draft.dsp.agc2.enabled,
    vad: draft.dsp.vadGate.enabled,
    limiter: true,
  };
  const quickDsp = status.settings.dsp;
  const quickControlsLocked = busy || quickDspBusy !== null;
  const agc2Bypassed =
    running &&
    quickDsp.agc2.enabled &&
    status.metrics.processingLoadMode === "reduced";

  const openProcessor = (stage: DspStageId) => {
    setActiveDspStage(stage);
    navigateToView("processing");
  };

  return (
    <div className="app-shell" data-engine-mode={status.mode}>
      <aside className="app-sidebar">
        <div className="brand-block" aria-label="XMic">
          <span className="brand-mark">
            <AudioWaveform size={22} strokeWidth={2.4} aria-hidden="true" />
          </span>
          <span className="brand-name">XMic</span>
        </div>

        <nav className="primary-navigation" aria-label={copy.navigation.workspace}>
          <span className="navigation-label">{copy.navigation.workspace}</span>
          <NavigationButton
            icon={<LayoutDashboard size={17} />}
            label={copy.navigation.monitor}
            active={activeView === "monitor"}
            onClick={() => navigateToView("monitor")}
          />
          <NavigationButton
            icon={<Route size={17} />}
            label={copy.navigation.routing}
            active={activeView === "routing"}
            onClick={() => navigateToView("routing")}
          />
          <NavigationButton
            icon={<SlidersHorizontal size={17} />}
            label={copy.navigation.processing}
            active={activeView === "processing"}
            onClick={() => navigateToView("processing")}
          />
        </nav>

        <div className="sidebar-spacer" />

        <section className="sidebar-engine" aria-label={copy.status.title}>
          <div className="sidebar-state">
            <span className="state-symbol" data-mode={status.mode}>
              <Activity size={15} aria-hidden="true" />
            </span>
            <span className="state-copy">
              <small>{copy.status.title}</small>
              <strong>{copy.engineModes[status.mode]}</strong>
            </span>
          </div>
          <button
            className={`engine-action ${engineRequested ? "is-stop" : "is-start"}`}
            type="button"
            onClick={toggleEngine}
            disabled={busy || quickDspBusy !== null}
          >
            {engineRequested ? <Square size={15} /> : <Play size={16} />}
            <span>
              {running
                ? copy.common.stop
                : engineRequested
                  ? copy.common.cancelPending
                  : copy.common.start}
            </span>
          </button>
        </section>

        <div
          className="language-switch"
          role="group"
          aria-label={copy.common.language}
          title={copy.common.language}
        >
          <Languages size={15} aria-hidden="true" />
          <button
            type="button"
            aria-pressed={language === "zh-CN"}
            onClick={() => setLanguage("zh-CN")}
          >
            中文
          </button>
          <button
            type="button"
            aria-pressed={language === "en"}
            onClick={() => setLanguage("en")}
          >
            EN
          </button>
        </div>
      </aside>

      <section className="app-main">
        <header className="page-header">
          <div className="page-title-block">
            <span>{copy.app.eyebrow}</span>
            <h1>XMic</h1>
            <p>{copy.app.subtitle}</p>
          </div>
          <div className="header-runtime" aria-label={copy.status.title}>
            <RuntimeItem
              icon={<Cpu size={14} />}
              label={copy.status.cpuLoad}
              value={copy.processingLoads[status.metrics.processingLoadMode]}
            />
            <RuntimeItem
              icon={<Clock3 size={14} />}
              label={copy.status.dspP99}
              value={`${status.metrics.processingTimeP99Us.toFixed(0)} µs`}
            />
          </div>
        </header>

        <div className="banner-stack">
          {error && (
            <div className="alert-banner error-banner" role="alert">
              <AlertTriangle size={16} />
              <span>{error}</span>
              {!running && engineRequested && (
                <button type="button" onClick={retryEngine} disabled={busy}>
                  <RefreshCw size={14} />
                  {copy.common.retryStart}
                </button>
              )}
            </div>
          )}
          {configurationIssue && configurationIssueMessage && (
            <button
              className="alert-banner configuration-banner"
              type="button"
              onClick={() => focusConfigurationIssue(configurationIssue)}
            >
              <AlertTriangle size={16} />
              <span>
                <strong>{copy.routing.configurationRequired}</strong>
                {configurationIssueMessage}
              </span>
              <ChevronRight size={15} aria-hidden="true" />
            </button>
          )}
          {status.warning && (
            <div className="alert-banner warning-banner" role="status">
              <AlertTriangle size={16} />
              <span>{status.warning}</span>
            </div>
          )}
          {(lifecycleStatus?.lastError ||
            lifecycleStatus?.sessionAvailable === false) && (
            <div className="alert-banner warning-banner" role="status">
              <AlertTriangle size={16} />
              <span>
                {lifecycleStatus.lastError ?? copy.app.sessionUnavailable}
              </span>
            </div>
          )}
        </div>

        <main
          className="page-content"
          onScroll={(event) => {
            const container = event.currentTarget;
            const marker =
              container.scrollTop + Math.min(container.clientHeight * 0.28, 180);
            let visibleView: ViewId = "monitor";
            for (const view of ["monitor", "routing", "processing"] as const) {
              const section = document.getElementById(`view-${view}`);
              if (section && section.offsetTop <= marker) visibleView = view;
            }
            setActiveView((current) =>
              current === visibleView ? current : visibleView,
            );
          }}
        >
          <section
            className="view-section"
            id="view-monitor"
            aria-labelledby="view-monitor-title"
          >
            <ViewSectionHeading
              id="view-monitor-title"
              icon={<LayoutDashboard size={18} />}
              eyebrow={copy.views.monitor.eyebrow}
              title={copy.views.monitor.title}
              subtitle={copy.views.monitor.subtitle}
            />
            <div className="monitor-view">
              <div className="monitor-grid">
                <section className="surface telemetry-panel">
                  <PanelHeading
                    icon={<Radio size={16} />}
                    title={copy.views.monitor.telemetryTitle}
                    subtitle={copy.views.monitor.telemetrySubtitle}
                    trailing={
                      <span className={`live-label${running ? " is-live" : ""}`}>
                        <span />
                        {running
                          ? copy.engineModes[status.mode]
                          : copy.engineModes.sleeping}
                      </span>
                    }
                  />
                  <TelemetryCanvas
                    metrics={status.metrics}
                    running={running}
                    label={copy.views.monitor.telemetryAria}
                  />
                  <div className="telemetry-legend">
                    <span data-series="vad">
                      <i />
                      {copy.views.monitor.vadLegend}
                    </span>
                    <span data-series="attenuation">
                      <i />
                      {copy.views.monitor.attenuationLegend}
                    </span>
                    <span data-series="gate">
                      <i />
                      {copy.views.monitor.gateWindow}
                    </span>
                  </div>
                  <div className="signal-readouts">
                    <SignalReadout
                      label={copy.views.monitor.inputPeak}
                      value={formatOptionalMetric(
                        status.metrics.inputPeakDbfs,
                        "dBFS",
                        1,
                      )}
                      tone="input"
                    />
                    <SignalReadout
                      label={copy.views.monitor.outputPeak}
                      value={formatOptionalMetric(
                        status.metrics.outputPeakDbfs,
                        "dBFS",
                        1,
                      )}
                      tone="output"
                    />
                    <SignalReadout
                      label={copy.views.monitor.vadProbability}
                      value={
                        !running || status.metrics.vadProbability == null
                          ? "--"
                          : `${Math.round(status.metrics.vadProbability * 100)}%`
                      }
                      tone="vad"
                    />
                  </div>
                </section>

                <section className="surface snapshot-panel">
                  <PanelHeading
                    icon={<CircleGauge size={16} />}
                    title={copy.views.monitor.snapshotTitle}
                    subtitle={copy.views.monitor.snapshotSubtitle}
                  />
                  <div className="mode-readout" data-mode={status.mode}>
                    <span>{copy.status.title}</span>
                    <strong>{copy.engineModes[status.mode]}</strong>
                    <small>{status.detail || copy.app.subtitle}</small>
                  </div>
                  <div className="snapshot-list">
                    <SnapshotItem
                      label={copy.views.monitor.pipelineLatency}
                      value={
                        running
                          ? formatOptionalMetric(
                              status.metrics.pipelineLatencyMs,
                              "ms",
                            )
                          : "--"
                      }
                    />
                    <SnapshotItem
                      label={copy.views.monitor.calibration}
                      value={
                        running
                          ? calibrationLabel(
                              status.metrics.hostDelayCalibrationState,
                              copy,
                            )
                          : "--"
                      }
                    />
                    <SnapshotItem
                      label={copy.views.monitor.gateState}
                      value={
                        running
                          ? booleanState(
                              status.metrics.vadGateOpen,
                              copy.dsp.diagnostics.open,
                              copy.dsp.diagnostics.closed,
                            )
                          : "--"
                      }
                    />
                    <SnapshotItem
                      label={copy.views.monitor.echoGuard}
                      value={
                        running
                          ? booleanState(
                              status.metrics.echoGuardActive,
                              copy.dsp.diagnostics.active,
                              copy.dsp.diagnostics.idle,
                            )
                          : "--"
                      }
                    />
                    <SnapshotItem
                      label={copy.status.cpuLoad}
                      value={copy.processingLoads[status.metrics.processingLoadMode]}
                    />
                  </div>
                </section>
              </div>

              <section className="surface route-map-panel">
                <PanelHeading
                  icon={<Network size={16} />}
                  title={copy.views.monitor.activeRoute}
                  subtitle={copy.views.monitor.activeRouteSubtitle}
                />
                <div className="route-map">
                  <div className="route-sources">
                    <RouteNode
                      icon={<Speaker size={16} />}
                      label={copy.views.monitor.renderSource}
                      value={routeName(visibleRenderId, status.endpoints)}
                      tone="render"
                    />
                    <RouteNode
                      icon={<Mic2 size={16} />}
                      label={copy.views.monitor.captureSource}
                      value={routeName(visibleCaptureId, status.endpoints)}
                      tone="capture"
                    />
                  </div>
                  <span className="route-connector merge-connector" aria-hidden="true">
                    <GitMerge size={17} />
                  </span>
                  <RouteNode
                    icon={<AudioWaveform size={17} />}
                    label={copy.views.monitor.processor}
                    value={profileLabel}
                    tone="processor"
                  />
                  <span className="route-connector" aria-hidden="true">
                    <ArrowRight size={17} />
                  </span>
                  <RouteNode
                    icon={<Cable size={16} />}
                    label={copy.views.monitor.outputDestination}
                    value={routeName(visibleOutputId, status.endpoints)}
                    tone="output"
                  />
                </div>
              </section>

              <div className="monitor-bottom-grid">
                <section className="surface chain-overview">
                  <PanelHeading
                    icon={<SlidersHorizontal size={16} />}
                    title={copy.views.monitor.chainTitle}
                    subtitle={copy.views.monitor.chainSubtitle}
                  />
                  <div
                    className="dsp-quick-grid"
                    aria-label={copy.views.monitor.quickControlsAria}
                  >
                    <QuickDspControl
                      label="AEC3"
                      description={copy.views.monitor.aecQuickDescription}
                      icon={<Waves size={15} />}
                      enabled={quickDsp.aec.enabled}
                      status={quickDspStatus(
                        quickDsp.aec.enabled,
                        false,
                        quickDspBusy === "aec",
                        copy,
                      )}
                      busy={quickDspBusy === "aec"}
                      controlsLocked={quickControlsLocked}
                      onOpen={() => openProcessor("aec")}
                      onToggle={(enabled) => setQuickDspFeature("aec", enabled)}
                      copy={copy}
                    />
                    <QuickDspControl
                      label="RNNoise"
                      description={copy.views.monitor.rnnoiseQuickDescription}
                      icon={<AudioLines size={15} />}
                      enabled={quickDsp.rnnoise.enabled}
                      status={quickDspStatus(
                        quickDsp.rnnoise.enabled,
                        false,
                        quickDspBusy === "rnnoise",
                        copy,
                      )}
                      busy={quickDspBusy === "rnnoise"}
                      controlsLocked={quickControlsLocked}
                      disabledReason={
                        quickDsp.rnnoise.enabled && quickDsp.vadGate.enabled
                          ? copy.views.monitor.rnnoiseRequiredByVad
                          : undefined
                      }
                      onOpen={() => openProcessor("rnnoise")}
                      onToggle={(enabled) =>
                        setQuickDspFeature("rnnoise", enabled)
                      }
                      copy={copy}
                    />
                    <QuickDspControl
                      label="AGC2"
                      description={copy.views.monitor.agc2QuickDescription}
                      icon={<Gauge size={15} />}
                      enabled={quickDsp.agc2.enabled}
                      bypassed={agc2Bypassed}
                      status={quickDspStatus(
                        quickDsp.agc2.enabled,
                        agc2Bypassed,
                        quickDspBusy === "agc2",
                        copy,
                      )}
                      busy={quickDspBusy === "agc2"}
                      controlsLocked={quickControlsLocked}
                      onOpen={() => openProcessor("agc2")}
                      onToggle={(enabled) => setQuickDspFeature("agc2", enabled)}
                      copy={copy}
                    />
                    <QuickDspControl
                      label="VAD Gate"
                      description={copy.views.monitor.vadQuickDescription}
                      icon={<Mic2 size={15} />}
                      enabled={quickDsp.vadGate.enabled}
                      status={quickDspStatus(
                        quickDsp.vadGate.enabled,
                        false,
                        quickDspBusy === "vadGate",
                        copy,
                      )}
                      busy={quickDspBusy === "vadGate"}
                      controlsLocked={quickControlsLocked}
                      disabledReason={
                        !quickDsp.vadGate.enabled && !quickDsp.rnnoise.enabled
                          ? copy.views.monitor.vadRequiresRnnoise
                          : undefined
                      }
                      onOpen={() => openProcessor("vad")}
                      onToggle={(enabled) =>
                        setQuickDspFeature("vadGate", enabled)
                      }
                      copy={copy}
                    />
                    <QuickDspControl
                      label="Limiter"
                      description={copy.views.monitor.limiterQuickDescription}
                      icon={<ShieldCheck size={15} />}
                      enabled
                      status={copy.views.monitor.alwaysEnabled}
                      locked
                      controlsLocked={quickControlsLocked}
                      onOpen={() => openProcessor("limiter")}
                      copy={copy}
                    />
                  </div>
                </section>

                <section className="surface health-panel">
                  <PanelHeading
                    icon={<Activity size={16} />}
                    title={copy.views.monitor.healthTitle}
                    subtitle={copy.views.monitor.healthSubtitle}
                  />
                  <div className="health-grid">
                    <MetricReadout
                      label={copy.status.processedBlocks}
                      value={status.metrics.processedBlocks.toLocaleString(language)}
                    />
                    <MetricReadout
                      label={copy.status.deviceRestarts}
                      value={status.metrics.deviceRestarts.toLocaleString(language)}
                    />
                    <MetricReadout
                      label={copy.status.outputQueue}
                      value={`${status.metrics.outputQueueDepthMs.toFixed(1)} ms`}
                      title={copy.status.outputQueueHelp}
                    />
                    <MetricReadout
                      label={copy.status.audioAnomalies}
                      value={audioAnomalyTotal(status.metrics).toLocaleString(language)}
                      title={copy.status.audioAnomaliesHelp}
                    />
                  </div>
                </section>
              </div>
            </div>
          </section>

          <section
            className="view-section"
            id="view-routing"
            aria-labelledby="view-routing-title"
          >
            <ViewSectionHeading
              id="view-routing-title"
              icon={<Route size={18} />}
              eyebrow={copy.views.routing.eyebrow}
              title={copy.views.routing.title}
              subtitle={copy.views.routing.subtitle}
            />
            <div className="routing-view">
              <section className="surface route-config-panel">
                <PanelHeading
                  icon={<Route size={16} />}
                  title={copy.views.routing.configuration}
                  subtitle={copy.views.routing.configurationSubtitle}
                  trailing={
                    <button
                      className="icon-button"
                      type="button"
                      title={copy.routing.refresh}
                      aria-label={copy.routing.refresh}
                      onClick={refresh}
                      disabled={busy}
                    >
                      <RefreshCw size={15} />
                    </button>
                  }
                />
                <div className="routing-controls">
                  <div
                    className="playback-reference-configuration"
                    id="playback-reference-configuration"
                  >
                    <Toggle
                      label={copy.routing.followDefaultRender}
                      help={copy.routing.followDefaultRenderHelp}
                      checked={draft.followDefaultRender}
                      disabled={controlsDisabled}
                      onChange={(checked) =>
                        changeSettings((current) => ({
                          ...current,
                          followDefaultRender: checked,
                          renderEndpointId: checked
                            ? null
                            : (current.renderEndpointId ?? defaultRender?.id ?? null),
                        }))
                      }
                    />

                    {!draft.followDefaultRender && (
                      <SelectControl
                        label={copy.routing.renderReference}
                        help={copy.routing.renderReferenceHelp}
                        value={draft.renderEndpointId ?? ""}
                        disabled={controlsDisabled}
                        onChange={(value) =>
                          changeSettings((current) => ({
                            ...current,
                            renderEndpointId: value || null,
                          }))
                        }
                      >
                        <option value="">{copy.routing.selectRender}</option>
                        {draft.renderEndpointId &&
                          !renderEndpoints.some(
                            (endpoint) => endpoint.id === draft.renderEndpointId,
                          ) && (
                            <option value={draft.renderEndpointId}>
                              {copy.routing.savedRenderUnavailable}
                            </option>
                          )}
                        {renderEndpoints.map((endpoint) => (
                          <option key={endpoint.id} value={endpoint.id}>
                            {endpoint.friendlyName}
                          </option>
                        ))}
                      </SelectControl>
                    )}
                  </div>

                  <SelectControl
                    label={copy.routing.captureInput}
                    help={copy.routing.captureInputHelp}
                    value={draft.captureEndpointId ?? ""}
                    disabled={controlsDisabled}
                    invalid={captureConfigurationIssue !== null}
                    ariaDescribedBy={
                      captureConfigurationIssue
                        ? "physical-capture-required"
                        : undefined
                    }
                    selectRef={captureSelectRef}
                    onChange={(value) =>
                      changeSettings((current) => ({
                        ...current,
                        captureEndpointId: value || null,
                      }))
                    }
                  >
                    <option
                      value=""
                      disabled={followDefaultCaptureBlocked}
                    >
                      {effectiveVirtualMode === "custom"
                        ? copy.routing.selectPhysicalCapture
                        : copy.routing.followDefaultCapture}
                    </option>
                    {draft.captureEndpointId &&
                      !captureEndpoints.some(
                        (endpoint) => endpoint.id === draft.captureEndpointId,
                      ) && (
                        <option value={draft.captureEndpointId}>
                          {copy.routing.savedCaptureUnavailable}
                        </option>
                      )}
                    {captureEndpoints.map((endpoint) => (
                      <option key={endpoint.id} value={endpoint.id}>
                        {endpoint.friendlyName}
                      </option>
                    ))}
                  </SelectControl>

                  {captureConfigurationIssue && (
                    <button
                      id="physical-capture-required"
                      className="configuration-blocker"
                      type="button"
                      onClick={focusPhysicalCapture}
                    >
                      <AlertTriangle size={15} aria-hidden="true" />
                      <span>
                        <strong>{copy.routing.physicalCaptureRequired}</strong>
                        <small>
                          {copy.routing.physicalCaptureRequiredHelp(
                            routeName(
                              captureConfigurationIssue.virtualCaptureEndpointId ??
                                undefined,
                              status.endpoints,
                            ),
                          )}
                        </small>
                      </span>
                      <ChevronRight size={15} aria-hidden="true" />
                    </button>
                  )}

                  <SelectControl
                    label={copy.routing.captureChannel}
                    help={copy.routing.captureChannelHelp}
                    value={
                      draft.captureChannel == null
                        ? "auto"
                        : String(draft.captureChannel)
                    }
                    disabled={controlsDisabled}
                    onChange={(value) =>
                      changeSettings((current) => ({
                        ...current,
                        captureChannel: value === "auto" ? null : Number(value),
                      }))
                    }
                  >
                    <option value="auto">Auto</option>
                    {draft.captureChannel != null && draft.captureChannel >= 16 && (
                      <option value={draft.captureChannel}>
                        {copy.routing.savedChannel(draft.captureChannel + 1)}
                      </option>
                    )}
                    {Array.from({ length: 16 }, (_, channel) => (
                      <option key={channel} value={channel}>
                        Channel {channel + 1}
                      </option>
                    ))}
                  </SelectControl>

                  <div
                    className="virtual-device-configuration"
                    id="virtual-device-configuration"
                  >
                    <div className="field-heading">
                      <ControlLabel
                        label={copy.routing.virtualDeviceMode}
                        help={copy.routing.virtualDeviceModeHelp}
                      />
                    </div>
                    <div className="segmented-control virtual-mode-control">
                      <button
                        type="button"
                        aria-pressed={effectiveVirtualMode === "vbCable"}
                        disabled={controlsDisabled || !vbCablePair}
                        onClick={() => selectVirtualMode("vbCable")}
                      >
                        {copy.routing.vbCableMode}
                      </button>
                      <button
                        type="button"
                        aria-pressed={effectiveVirtualMode === "custom"}
                        disabled={controlsDisabled}
                        onClick={() => selectVirtualMode("custom")}
                      >
                        {copy.routing.customVirtualMode}
                      </button>
                    </div>

                    {(customPairOffline || !vbCablePair) && (
                      <div
                        className={`virtual-device-notice${
                          customPairTrusted ? " is-advisory" : ""
                        }`}
                        role={customPairTrusted ? "status" : "alert"}
                      >
                        <AlertTriangle size={17} aria-hidden="true" />
                        <div>
                          <strong>
                            {customPairOffline
                              ? copy.routing.customDeviceWaiting
                              : customPairTrusted
                              ? copy.routing.customDeviceActive
                              : copy.routing.vbCableMissing}
                          </strong>
                          <p>
                            {customPairOffline
                              ? copy.routing.customDeviceWaitingHelp
                              : customPairTrusted
                              ? copy.routing.customDeviceActiveHelp
                              : copy.routing.vbCableMissingHelp}
                          </p>
                          {!customPairTrusted && (
                            <ol>
                              <li>{copy.routing.vbCableInstallStepDownload}</li>
                              <li>{copy.routing.vbCableInstallStepAdmin}</li>
                              <li>{copy.routing.vbCableInstallStepRefresh}</li>
                            </ol>
                          )}
                          <div className="inline-actions">
                            {!customPairTrusted && (
                              <button
                                type="button"
                                onClick={openVbCableDownload}
                              >
                                <ExternalLink size={14} aria-hidden="true" />
                                {copy.routing.openVbCableDownload}
                              </button>
                            )}
                            <button
                              type="button"
                              onClick={copyDeviceDiagnostics}
                            >
                              {diagnosticsCopied ? (
                                <CircleCheck size={14} aria-hidden="true" />
                              ) : (
                                <Copy size={14} aria-hidden="true" />
                              )}
                              {diagnosticsCopied
                                ? copy.routing.diagnosticsCopied
                                : copy.routing.copyDeviceDiagnostics}
                            </button>
                          </div>
                        </div>
                      </div>
                    )}

                    {effectiveVirtualMode === "vbCable" && vbCablePair ? (
                      <div className="detected-device-pair">
                        <CircleCheck size={17} aria-hidden="true" />
                        <div>
                          <strong>{copy.routing.vbCableDetected}</strong>
                          <span>
                            {vbCablePair.render.friendlyName}
                            <ArrowRight size={13} aria-hidden="true" />
                            {vbCablePair.capture.friendlyName}
                          </span>
                        </div>
                      </div>
                    ) : (
                      <div className="custom-virtual-pair">
                        <SelectControl
                          label={copy.routing.customVirtualRender}
                          help={copy.routing.customVirtualRenderHelp}
                          value={
                            draft.virtualDeviceMode === "custom"
                              ? (draft.outputEndpointId ?? "")
                              : ""
                          }
                          disabled={controlsDisabled}
                          invalid={
                            virtualConfigurationIssue?.reason ===
                            "customPairIncomplete"
                          }
                          selectRef={virtualOutputSelectRef}
                          onChange={(value) => {
                            const output = renderEndpoints.find(
                              (endpoint) => endpoint.id === value,
                            );
                            const inferredCapture = output
                              ? inferPairedCapture(
                                  output,
                                  customCaptureEndpoints,
                                )
                              : undefined;
                            changeSettings((current) => ({
                              ...current,
                              virtualDeviceMode: "custom",
                              outputEndpointId: value || null,
                              virtualCaptureEndpointId:
                                inferredCapture?.id ??
                                (current.virtualDeviceMode === "custom"
                                  ? current.virtualCaptureEndpointId
                                  : null),
                            }));
                          }}
                        >
                          <option value="">
                            {copy.routing.selectCustomVirtualRender}
                          </option>
                          {draft.virtualDeviceMode === "custom" &&
                            draft.outputEndpointId &&
                            !renderEndpoints.some(
                              (endpoint) =>
                                endpoint.id === draft.outputEndpointId,
                            ) && (
                              <option value={draft.outputEndpointId}>
                                {copy.routing.savedOutputUnavailable}
                              </option>
                            )}
                          {otherOutputs.map((endpoint) => (
                            <option key={endpoint.id} value={endpoint.id}>
                              {endpoint.friendlyName}
                            </option>
                          ))}
                        </SelectControl>

                        <SelectControl
                          label={copy.routing.customVirtualCapture}
                          help={copy.routing.customVirtualCaptureHelp}
                          value={
                            draft.virtualDeviceMode === "custom"
                              ? (draft.virtualCaptureEndpointId ?? "")
                              : ""
                          }
                          disabled={controlsDisabled}
                          invalid={
                            virtualConfigurationIssue?.reason ===
                            "customPairIncomplete"
                          }
                          onChange={(value) =>
                            changeSettings((current) => ({
                              ...current,
                              virtualDeviceMode: "custom",
                              virtualCaptureEndpointId: value || null,
                            }))
                          }
                        >
                          <option value="">
                            {copy.routing.selectCustomVirtualCapture}
                          </option>
                          {draft.virtualDeviceMode === "custom" &&
                            draft.virtualCaptureEndpointId &&
                            !allCaptureEndpoints.some(
                              (endpoint) =>
                                endpoint.id ===
                                draft.virtualCaptureEndpointId,
                            ) && (
                              <option value={draft.virtualCaptureEndpointId}>
                                {copy.routing.savedVirtualCaptureUnavailable}
                              </option>
                            )}
                          {customCaptureEndpoints.map((endpoint) => (
                            <option key={endpoint.id} value={endpoint.id}>
                              {endpoint.friendlyName}
                            </option>
                          ))}
                        </SelectControl>

                        <label
                          className={`custom-pair-confirmation${
                            customPairTrusted ? " is-confirmed" : ""
                          }`}
                        >
                          <input
                            type="checkbox"
                            checked={customPairTrusted}
                            disabled={
                              busy ||
                              !draft.outputEndpointId ||
                              !draft.virtualCaptureEndpointId
                            }
                            onChange={(event) => {
                              if (event.target.checked) {
                                confirmCustomPair();
                                return;
                              }
                              const pair = draft.trustedVirtualAudioPairs.find(
                                (candidate) =>
                                  candidate.renderEndpointId ===
                                    draft.outputEndpointId &&
                                  candidate.captureEndpointId ===
                                    draft.virtualCaptureEndpointId,
                              );
                              if (pair) void revokeTrustedPair(pair);
                            }}
                          />
                          <span>
                            <strong>{copy.routing.confirmCustomPair}</strong>
                            <small>{copy.routing.confirmCustomPairHelp}</small>
                          </span>
                        </label>

                        {virtualConfigurationIssue && (
                          <div className="virtual-configuration-error" role="alert">
                            <AlertTriangle size={15} aria-hidden="true" />
                            <span>
                              {virtualConfigurationIssue.reason ===
                              "customPairUnconfirmed"
                                ? copy.routing.customPairUnconfirmed
                                : copy.routing.customPairIncomplete}
                            </span>
                          </div>
                        )}

                        {draft.trustedVirtualAudioPairs.length > 0 && (
                          <details className="trusted-device-manager">
                            <summary>
                              {copy.routing.manageTrustedDevices} ·{" "}
                              {draft.trustedVirtualAudioPairs.length}
                            </summary>
                            <div className="trusted-device-list">
                              {draft.trustedVirtualAudioPairs.map((pair) => (
                                <div
                                  className="trusted-device-row"
                                  key={`${pair.renderEndpointId}:${pair.captureEndpointId}`}
                                >
                                  <span>
                                    <strong>{pair.renderName}</strong>
                                    <small>{pair.captureName}</small>
                                  </span>
                                  <button
                                    className="icon-button"
                                    type="button"
                                    title={copy.routing.revokeTrustedDevice}
                                    aria-label={copy.routing.revokeTrustedDevice}
                                    disabled={busy}
                                    onClick={() => void revokeTrustedPair(pair)}
                                  >
                                    <Trash2 size={14} aria-hidden="true" />
                                  </button>
                                </div>
                              ))}
                            </div>
                          </details>
                        )}
                      </div>
                    )}
                  </div>
                </div>
              </section>

              <div className="routing-side-stack">
                <section className="surface current-route-panel">
                  <PanelHeading
                    icon={<Network size={16} />}
                    title={copy.views.routing.active}
                    subtitle={copy.views.routing.activeSubtitle}
                  />
                  <div className="route-lines">
                    <RouteLine
                      icon={<Speaker size={14} />}
                      label={copy.routing.render}
                      value={routeName(
                        status.activeRoute?.renderEndpointId,
                        status.endpoints,
                      )}
                    />
                    <RouteLine
                      icon={<Mic2 size={14} />}
                      label={copy.routing.windowsDefaultCapture}
                      value={defaultCapture?.friendlyName ?? "-"}
                    />
                    <RouteLine
                      icon={<Mic2 size={14} />}
                      label={copy.routing.xmicPhysicalCapture}
                      value={
                        selectedCapture?.friendlyName ?? copy.routing.notSelected
                      }
                    />
                    <RouteLine
                      icon={<Cable size={14} />}
                      label={copy.routing.output}
                      value={routeName(
                        status.activeRoute?.outputEndpointId,
                        status.endpoints,
                      )}
                    />
                    <RouteLine
                      icon={<SlidersHorizontal size={14} />}
                      label={copy.routing.dspProfile}
                      value={profileLabel}
                    />
                  </div>
                </section>

                <section className="surface policy-panel">
                  <PanelHeading
                    icon={<ShieldCheck size={16} />}
                    title={copy.views.routing.safety}
                    subtitle={copy.views.routing.safetySubtitle}
                  />
                  <div className="field-heading">
                    <ControlLabel
                      label={copy.routing.failurePolicy}
                      help={copy.routing.failurePolicyHelp}
                    />
                  </div>
                  <div className="segmented-control">
                    <button
                      type="button"
                      aria-pressed={draft.failurePolicy === "rawPassthrough"}
                      disabled={controlsDisabled}
                      onClick={() =>
                        changeSettings((current) => ({
                          ...current,
                          failurePolicy: "rawPassthrough",
                        }))
                      }
                    >
                      {copy.routing.rawPassthrough}
                    </button>
                    <button
                      type="button"
                      aria-pressed={draft.failurePolicy === "mute"}
                      disabled={controlsDisabled}
                      onClick={() =>
                        changeSettings((current) => ({
                          ...current,
                          failurePolicy: "mute",
                        }))
                      }
                    >
                      {copy.routing.mute}
                    </button>
                  </div>
                </section>

                <section className="surface lifecycle-panel">
                  <PanelHeading
                    icon={<Power size={16} />}
                    title={copy.views.routing.lifecycle}
                    subtitle={copy.views.routing.lifecycleSubtitle}
                  />
                  <div className="lifecycle-controls">
                    <Toggle
                      label={copy.routing.startInTray}
                      help={copy.routing.startInTrayHelp}
                      checked={lifecycleStatus?.startInTray ?? false}
                      disabled={!lifecycleStatus || lifecycleBusy}
                      onChange={(startInTray) =>
                        updateLifecyclePreferences({ startInTray })
                      }
                    />
                    <Toggle
                      label={copy.routing.launchAtLogin}
                      help={copy.routing.launchAtLoginHelp}
                      checked={lifecycleStatus?.launchAtLogin ?? false}
                      disabled={!lifecycleStatus || lifecycleBusy}
                      onChange={(launchAtLogin) =>
                        updateLifecyclePreferences({ launchAtLogin })
                      }
                    />
                    <Toggle
                      label={copy.routing.startOnLaunch}
                      help={
                        configurationIssue
                          ? copy.routing.startOnLaunchBlockedHelp
                          : copy.routing.startOnLaunchHelp
                      }
                      checked={lifecycleStatus?.startEngineOnLaunch ?? false}
                      disabled={
                        !lifecycleStatus ||
                        lifecycleBusy ||
                        Boolean(
                          configurationIssue &&
                            !lifecycleStatus.startEngineOnLaunch,
                        )
                      }
                      onChange={(startEngineOnLaunch) =>
                        updateLifecyclePreferences({ startEngineOnLaunch })
                      }
                    />
                  </div>
                </section>
              </div>
            </div>
          </section>

          <section
            className="view-section"
            id="view-processing"
            aria-labelledby="view-processing-title"
          >
            <ViewSectionHeading
              id="view-processing-title"
              icon={<SlidersHorizontal size={18} />}
              eyebrow={copy.views.processing.eyebrow}
              title={copy.views.processing.title}
              subtitle={copy.views.processing.subtitle}
            />
            <div className="processing-view">
              <div
                className="processor-tabs"
                role="tablist"
                aria-label={copy.views.processing.pipelineAria}
              >
                <ProcessorTab
                  label="AEC3"
                  subtitle={copy.dsp.aecSubtitle}
                  icon={<Waves size={17} />}
                  active={activeDspStage === "aec"}
                  enabled={stageEnabled.aec}
                  onClick={() => setActiveDspStage("aec")}
                />
                <ProcessorTab
                  label="RNNoise"
                  subtitle={copy.dsp.rnnoiseSubtitle}
                  icon={<AudioLines size={17} />}
                  active={activeDspStage === "rnnoise"}
                  enabled={stageEnabled.rnnoise}
                  onClick={() => setActiveDspStage("rnnoise")}
                />
                <ProcessorTab
                  label="AGC2"
                  subtitle={copy.dsp.agc2Subtitle}
                  icon={<Gauge size={17} />}
                  active={activeDspStage === "agc2"}
                  enabled={stageEnabled.agc2}
                  onClick={() => setActiveDspStage("agc2")}
                />
                <ProcessorTab
                  label="VAD Gate"
                  subtitle={copy.dsp.vadSubtitle}
                  icon={<Mic2 size={17} />}
                  active={activeDspStage === "vad"}
                  enabled={stageEnabled.vad}
                  onClick={() => setActiveDspStage("vad")}
                />
                <ProcessorTab
                  label="Limiter"
                  subtitle={copy.dsp.limiterSubtitle}
                  icon={<ShieldCheck size={17} />}
                  active={activeDspStage === "limiter"}
                  enabled
                  onClick={() => setActiveDspStage("limiter")}
                />
              </div>

              <section className="surface processor-editor">
                <div className="processor-editor-heading">
                  <span className="processor-index">
                    {String(
                      ["aec", "rnnoise", "agc2", "vad", "limiter"].indexOf(
                        activeDspStage,
                      ) + 1,
                    ).padStart(2, "0")}
                  </span>
                  <span className="processor-icon">
                    {stageIcon(activeDspStage, 19)}
                  </span>
                  <div className="processor-title">
                    <span>{copy.views.processing.editorLabel}</span>
                    <h2>{stageName(activeDspStage)}</h2>
                    <p>{stageSubtitle(activeDspStage, copy)}</p>
                  </div>
                  <div className="processor-toggle">
                    {activeDspStage === "aec" && (
                      <Toggle
                        compact
                        label={copy.dsp.aec.enabled}
                        help={copy.dsp.aec.enabledHelp}
                        checked={draft.dsp.aec.enabled}
                        disabled={controlsDisabled}
                        onChange={(enabled) => updateAec({ enabled })}
                      />
                    )}
                    {activeDspStage === "rnnoise" && (
                      <Toggle
                        compact
                        label={copy.dsp.rnnoise.enabled}
                        help={copy.dsp.rnnoise.enabledHelp}
                        checked={draft.dsp.rnnoise.enabled}
                        disabled={controlsDisabled || draft.dsp.vadGate.enabled}
                        onChange={(enabled) => updateRnnoise({ enabled })}
                      />
                    )}
                    {activeDspStage === "agc2" && (
                      <Toggle
                        compact
                        label={copy.dsp.agc2.enabled}
                        help={copy.dsp.agc2.enabledHelp}
                        checked={draft.dsp.agc2.enabled}
                        disabled={controlsDisabled}
                        onChange={(enabled) => enableAgc2(enabled)}
                      />
                    )}
                    {activeDspStage === "vad" && (
                      <Toggle
                        compact
                        label={copy.dsp.vad.enabled}
                        help={copy.dsp.vad.enabledHelp}
                        checked={draft.dsp.vadGate.enabled}
                        disabled={controlsDisabled || !draft.dsp.rnnoise.enabled}
                        onChange={(enabled) => updateVad({ enabled })}
                      />
                    )}
                    {activeDspStage === "limiter" && (
                      <span className="always-on">
                        <ShieldCheck size={14} />
                        {copy.views.processing.alwaysOn}
                      </span>
                    )}
                  </div>
                </div>

                {running && (
                  <div className="editor-lock-notice">
                    <Activity size={14} />
                    {copy.views.processing.stopToEdit}
                  </div>
                )}

                <div className="processor-editor-body">
                  {activeDspStage === "aec" && (
                    <>
                      <div className="editor-control-grid aec-control-grid">
                        <div className="segmented-field">
                          <ControlLabel
                            label={copy.dsp.aec.preset}
                            help={copy.dsp.aec.presetHelp}
                          />
                          <div className="segmented-control">
                            {(["natural", "balanced", "strong"] as AecPreset[]).map(
                              (preset) => (
                                <button
                                  key={preset}
                                  type="button"
                                  aria-pressed={draft.dsp.aec.preset === preset}
                                  disabled={controlsDisabled || !draft.dsp.aec.enabled}
                                  onClick={() => updateAec({ preset })}
                                >
                                  {preset === "natural"
                                    ? "Natural"
                                    : preset === "balanced"
                                      ? "Balanced"
                                      : "Strong"}
                                </button>
                              ),
                            )}
                          </div>
                        </div>
                        <div className="single-toggle-field">
                          <Toggle
                            label={copy.dsp.aec.highPass}
                            help={copy.dsp.aec.highPassHelp}
                            checked={draft.dsp.aec.highPassFilterEnabled}
                            disabled={controlsDisabled || !draft.dsp.aec.enabled}
                            onChange={(highPassFilterEnabled) =>
                              updateAec({ highPassFilterEnabled })
                            }
                          />
                        </div>
                      </div>
                      <details className="diagnostics-panel">
                        <summary>
                          <span className="summary-icon">
                            <Activity size={15} />
                          </span>
                          <span>
                            <strong>{copy.views.processing.diagnostics}</strong>
                            <small>{copy.views.processing.diagnosticsSubtitle}</small>
                          </span>
                          <ChevronRight size={15} className="summary-chevron" />
                        </summary>
                        <AecDiagnostics
                          metrics={status.metrics}
                          copy={copy}
                          language={language}
                        />
                      </details>
                    </>
                  )}

                  {activeDspStage === "rnnoise" && (
                    <div className="focused-control">
                      <RangeControl
                        label={copy.dsp.rnnoise.mix}
                        help={copy.dsp.rnnoise.mixHelp}
                        value={Math.round(draft.dsp.rnnoise.mix * 100)}
                        minimum={0}
                        maximum={100}
                        step={5}
                        suffix="%"
                        disabled={controlsDisabled || !draft.dsp.rnnoise.enabled}
                        onChange={(value) => updateRnnoise({ mix: value / 100 })}
                      />
                    </div>
                  )}

                  {activeDspStage === "agc2" && (
                    <>
                      <div className="single-toggle-field">
                        <Toggle
                          label={copy.dsp.agc2.adaptive}
                          help={copy.dsp.agc2.adaptiveHelp}
                          checked={draft.dsp.agc2.adaptiveDigitalEnabled}
                          disabled={
                            controlsDisabled ||
                            !draft.dsp.agc2.enabled ||
                            (draft.dsp.agc2.adaptiveDigitalEnabled &&
                              draft.dsp.agc2.fixedGainDb <= 0)
                          }
                          onChange={(adaptiveDigitalEnabled) =>
                            updateAgc2({ adaptiveDigitalEnabled })
                          }
                        />
                      </div>
                      <div className="parameter-grid">
                        <NumberControl
                          label={copy.dsp.agc2.fixedGain}
                          help={copy.dsp.agc2.fixedGainHelp}
                          value={draft.dsp.agc2.fixedGainDb}
                          minimum={0}
                          maximum={30}
                          step={0.5}
                          suffix="dB"
                          disabled={controlsDisabled || !draft.dsp.agc2.enabled}
                          onChange={(fixedGainDb) => updateAgc2({ fixedGainDb })}
                        />
                        <NumberControl
                          label={copy.dsp.agc2.headroom}
                          help={copy.dsp.agc2.headroomHelp}
                          value={draft.dsp.agc2.headroomDb}
                          minimum={0}
                          maximum={30}
                          step={0.5}
                          suffix="dB"
                          disabled={controlsDisabled || !draft.dsp.agc2.enabled}
                          onChange={(headroomDb) => updateAgc2({ headroomDb })}
                        />
                        <NumberControl
                          label={copy.dsp.agc2.maxGain}
                          help={copy.dsp.agc2.maxGainHelp}
                          value={draft.dsp.agc2.maxGainDb}
                          minimum={0.1}
                          maximum={50}
                          step={0.5}
                          suffix="dB"
                          disabled={controlsDisabled || !draft.dsp.agc2.enabled}
                          onChange={(maxGainDb) => updateAgc2({ maxGainDb })}
                        />
                        <NumberControl
                          label={copy.dsp.agc2.initialGain}
                          help={copy.dsp.agc2.initialGainHelp}
                          value={draft.dsp.agc2.initialGainDb}
                          minimum={0}
                          maximum={draft.dsp.agc2.maxGainDb}
                          step={0.5}
                          suffix="dB"
                          disabled={controlsDisabled || !draft.dsp.agc2.enabled}
                          onChange={(initialGainDb) => updateAgc2({ initialGainDb })}
                        />
                        <NumberControl
                          label={copy.dsp.agc2.gainSpeed}
                          help={copy.dsp.agc2.gainSpeedHelp}
                          value={draft.dsp.agc2.maxGainChangeDbPerSecond}
                          minimum={0.1}
                          maximum={20}
                          step={0.5}
                          suffix="dB/s"
                          disabled={controlsDisabled || !draft.dsp.agc2.enabled}
                          onChange={(maxGainChangeDbPerSecond) =>
                            updateAgc2({ maxGainChangeDbPerSecond })
                          }
                        />
                        <NumberControl
                          label={copy.dsp.agc2.noiseLimit}
                          help={copy.dsp.agc2.noiseLimitHelp}
                          value={draft.dsp.agc2.maxOutputNoiseLevelDbfs}
                          minimum={-90}
                          maximum={0}
                          step={1}
                          suffix="dBFS"
                          disabled={controlsDisabled || !draft.dsp.agc2.enabled}
                          onChange={(maxOutputNoiseLevelDbfs) =>
                            updateAgc2({ maxOutputNoiseLevelDbfs })
                          }
                        />
                      </div>
                    </>
                  )}

                  {activeDspStage === "vad" && (
                    <>
                      <div className="latency-estimate">
                        <span>{copy.dsp.vad.latencyEstimate}</span>
                        <strong>{`${estimatedDspLatencyMs(draft.dsp)} ms`}</strong>
                        <small>{copy.dsp.vad.latencyEstimateHelp}</small>
                      </div>
                      <div className="focused-control">
                        <RangeControl
                          label={copy.dsp.vad.threshold}
                          help={copy.dsp.vad.thresholdHelp}
                          value={draft.dsp.vadGate.threshold}
                          minimum={0}
                          maximum={1}
                          step={0.05}
                          suffix=""
                          disabled={controlsDisabled || !draft.dsp.vadGate.enabled}
                          onChange={(threshold) => updateVad({ threshold })}
                        />
                      </div>
                      <div className="parameter-grid">
                        <NumberControl
                          label={copy.dsp.vad.confirmation}
                          help={copy.dsp.vad.confirmationHelp}
                          value={draft.dsp.vadGate.confirmationMs}
                          minimum={10}
                          maximum={1000}
                          step={10}
                          suffix="ms"
                          disabled={controlsDisabled || !draft.dsp.vadGate.enabled}
                          onChange={(confirmationMs) => updateVad({ confirmationMs })}
                        />
                        <NumberControl
                          label={copy.dsp.vad.hold}
                          help={copy.dsp.vad.holdHelp}
                          value={draft.dsp.vadGate.holdMs}
                          minimum={0}
                          maximum={10000}
                          step={10}
                          suffix="ms"
                          disabled={controlsDisabled || !draft.dsp.vadGate.enabled}
                          onChange={(holdMs) => updateVad({ holdMs })}
                        />
                        <NumberControl
                          label={copy.dsp.vad.floor}
                          help={copy.dsp.vad.floorHelp}
                          value={draft.dsp.vadGate.floorDb}
                          minimum={-90}
                          maximum={0}
                          step={1}
                          suffix="dB"
                          disabled={controlsDisabled || !draft.dsp.vadGate.enabled}
                          onChange={(floorDb) => updateVad({ floorDb })}
                        />
                        <NumberControl
                          label={copy.dsp.vad.attack}
                          help={copy.dsp.vad.attackHelp}
                          value={draft.dsp.vadGate.attackMs}
                          minimum={0.1}
                          maximum={10000}
                          step={1}
                          suffix="ms"
                          disabled={controlsDisabled || !draft.dsp.vadGate.enabled}
                          onChange={(attackMs) => updateVad({ attackMs })}
                        />
                        <NumberControl
                          label={copy.dsp.vad.release}
                          help={copy.dsp.vad.releaseHelp}
                          value={draft.dsp.vadGate.releaseMs}
                          minimum={0.1}
                          maximum={10000}
                          step={1}
                          suffix="ms"
                          disabled={controlsDisabled || !draft.dsp.vadGate.enabled}
                          onChange={(releaseMs) => updateVad({ releaseMs })}
                        />
                      </div>
                    </>
                  )}

                  {activeDspStage === "limiter" && (
                    <div className="parameter-grid limiter-parameters">
                      <NumberControl
                        label={copy.dsp.limiter.ceiling}
                        help={copy.dsp.limiter.ceilingHelp}
                        value={draft.dsp.limiter.ceilingDbfs}
                        minimum={-30}
                        maximum={0}
                        step={0.5}
                        suffix="dBFS"
                        disabled={controlsDisabled}
                        onChange={(ceilingDbfs) =>
                          updateLimiter({ ceilingDbfs })
                        }
                      />
                      <NumberControl
                        label={copy.dsp.limiter.release}
                        help={copy.dsp.limiter.releaseHelp}
                        value={draft.dsp.limiter.releaseMs}
                        minimum={0.1}
                        maximum={10000}
                        step={1}
                        suffix="ms"
                        disabled={controlsDisabled}
                        onChange={(releaseMs) => updateLimiter({ releaseMs })}
                      />
                    </div>
                  )}
                </div>
              </section>
            </div>
          </section>
        </main>

        {dirty && (
          <div className="apply-dock" role="status">
            <span className="pending-indicator" aria-hidden="true" />
            <span>{copy.common.pendingSettings}</span>
            <button
              type="button"
              onClick={applySettings}
              disabled={controlsDisabled}
            >
              <Save size={15} />
              {copy.common.applySettings}
            </button>
          </div>
        )}
      </section>
    </div>
  );
}

function ViewSectionHeading({
  id,
  icon,
  eyebrow,
  title,
  subtitle,
}: {
  id: string;
  icon: ReactNode;
  eyebrow: string;
  title: string;
  subtitle: string;
}) {
  return (
    <header className="view-section-heading">
      <span className="view-section-icon">{icon}</span>
      <span className="view-section-copy">
        <small>{eyebrow}</small>
        <h2 id={id}>{title}</h2>
        <p>{subtitle}</p>
      </span>
    </header>
  );
}

function NavigationButton({
  icon,
  label,
  active,
  onClick,
}: {
  icon: ReactNode;
  label: string;
  active: boolean;
  onClick: () => void;
}) {
  return (
    <button
      className="navigation-button"
      type="button"
      aria-current={active ? "page" : undefined}
      title={label}
      onClick={onClick}
    >
      <span className="navigation-icon">{icon}</span>
      <span>{label}</span>
      <ChevronRight size={14} className="navigation-chevron" />
    </button>
  );
}

function RuntimeItem({
  icon,
  label,
  value,
}: {
  icon: ReactNode;
  label: string;
  value: string;
}) {
  return (
    <div className="runtime-item">
      <span className="runtime-icon">{icon}</span>
      <span>
        <small>{label}</small>
        <strong>{value}</strong>
      </span>
    </div>
  );
}

function PanelHeading({
  icon,
  title,
  subtitle,
  trailing,
}: {
  icon: ReactNode;
  title: string;
  subtitle: string;
  trailing?: ReactNode;
}) {
  return (
    <div className="panel-heading">
      <span className="panel-icon">{icon}</span>
      <span className="panel-title">
        <strong>{title}</strong>
        <small>{subtitle}</small>
      </span>
      {trailing && <span className="panel-trailing">{trailing}</span>}
    </div>
  );
}

function SignalReadout({
  label,
  value,
  tone,
}: {
  label: string;
  value: string;
  tone: "input" | "output" | "vad";
}) {
  return (
    <div className="signal-readout" data-tone={tone}>
      <span>{label}</span>
      <strong>{value}</strong>
    </div>
  );
}

function SnapshotItem({ label, value }: { label: string; value: string }) {
  return (
    <div className="snapshot-item">
      <span>{label}</span>
      <strong>{value}</strong>
    </div>
  );
}

function RouteNode({
  icon,
  label,
  value,
  tone,
}: {
  icon: ReactNode;
  label: string;
  value: string;
  tone: "render" | "capture" | "processor" | "output";
}) {
  return (
    <div className="route-node" data-tone={tone}>
      <span className="route-node-icon">{icon}</span>
      <span className="route-node-copy">
        <small>{label}</small>
        <strong title={value}>{value}</strong>
      </span>
    </div>
  );
}

function QuickDspControl({
  label,
  description,
  icon,
  enabled,
  bypassed = false,
  status,
  busy = false,
  locked = false,
  controlsLocked,
  disabledReason,
  onOpen,
  onToggle,
  copy,
}: {
  label: string;
  description: string;
  icon: ReactNode;
  enabled: boolean;
  bypassed?: boolean;
  status: string;
  busy?: boolean;
  locked?: boolean;
  controlsLocked: boolean;
  disabledReason?: string;
  onOpen: () => void;
  onToggle?: (enabled: boolean) => void;
  copy: Translation;
}) {
  const dependencyBlocked = Boolean(disabledReason);
  const state = busy
    ? "busy"
    : bypassed
      ? "bypassed"
      : locked
        ? "locked"
        : enabled
          ? "enabled"
          : "disabled";

  return (
    <div
      className="dsp-quick-control"
      data-enabled={enabled}
      data-state={state}
    >
      <button
        className="dsp-module-open"
        type="button"
        aria-label={copy.views.monitor.openModule(label)}
        onClick={onOpen}
      >
        <span className="dsp-module-icon">{icon}</span>
        <span className="dsp-module-copy">
          <strong>{label}</strong>
          <small>{description}</small>
        </span>
        <ChevronRight size={13} aria-hidden="true" />
      </button>
      <span className="dsp-module-action">
        <small className="dsp-module-status">{status}</small>
        {locked ? (
          <span
            className="dsp-module-lock"
            title={copy.views.monitor.alwaysEnabled}
            aria-label={`${label}: ${copy.views.monitor.alwaysEnabled}`}
          >
            <LockKeyhole size={14} aria-hidden="true" />
          </span>
        ) : (
          <button
            className="dsp-quick-switch"
            type="button"
            role="switch"
            aria-checked={enabled}
            aria-disabled={dependencyBlocked || undefined}
            aria-label={copy.views.monitor.toggleModule(label)}
            title={
              disabledReason ?? copy.views.monitor.toggleModule(label)
            }
            disabled={controlsLocked}
            onClick={() => {
              if (!dependencyBlocked && !controlsLocked) onToggle?.(!enabled);
            }}
          >
            {busy ? (
              <LoaderCircle
                className="dsp-switch-spinner"
                size={13}
                aria-hidden="true"
              />
            ) : (
              <span aria-hidden="true" />
            )}
          </button>
        )}
      </span>
    </div>
  );
}

function MetricReadout({
  label,
  value,
  title,
}: {
  label: string;
  value: string;
  title?: string;
}) {
  return (
    <div className="metric-readout" title={title}>
      <span>{label}</span>
      <strong>{value}</strong>
    </div>
  );
}

function RouteLine({
  icon,
  label,
  value,
}: {
  icon: ReactNode;
  label: string;
  value: string;
}) {
  return (
    <div className="route-line">
      <span className="route-line-icon">{icon}</span>
      <span title={label}>{label}</span>
      <strong title={value}>{value}</strong>
    </div>
  );
}

function ProcessorTab({
  label,
  subtitle,
  icon,
  active,
  enabled,
  onClick,
}: {
  label: string;
  subtitle: string;
  icon: ReactNode;
  active: boolean;
  enabled: boolean;
  onClick: () => void;
}) {
  return (
    <button
      className="processor-tab"
      type="button"
      role="tab"
      aria-selected={active}
      data-enabled={enabled}
      onClick={onClick}
    >
      <span className="processor-tab-icon">{icon}</span>
      <span className="processor-tab-copy">
        <strong>{label}</strong>
        <small>{subtitle}</small>
      </span>
      <i aria-hidden="true" />
    </button>
  );
}

function AecDiagnostics({
  metrics,
  copy,
  language,
}: {
  metrics: EngineMetrics;
  copy: Translation;
  language: Language;
}) {
  return (
    <div className="diagnostics-grid" aria-label={copy.dsp.aec.diagnosticsAria}>
      <DiagnosticCell
        label={copy.dsp.diagnostics.pipelineLatency}
        value={formatOptionalMetric(metrics.pipelineLatencyMs, "ms")}
        title={copy.dsp.diagnostics.pipelineLatencyHelp}
      />
      <DiagnosticCell
        label={copy.dsp.diagnostics.captureLatency}
        value={formatOptionalMetric(metrics.captureLatencyMs, "ms", 1)}
        title={copy.dsp.diagnostics.captureLatencyHelp}
      />
      <DiagnosticCell
        label={copy.dsp.diagnostics.dspLatency}
        value={formatOptionalMetric(metrics.dspLatencyMs, "ms")}
        title={copy.dsp.diagnostics.dspLatencyHelp}
      />
      <DiagnosticCell
        label={copy.dsp.diagnostics.outputLatency}
        value={formatOptionalMetric(metrics.outputLatencyMs, "ms", 1)}
        title={copy.dsp.diagnostics.outputLatencyHelp}
      />
      <DiagnosticCell
        label={copy.dsp.diagnostics.hostDelay}
        value={formatOptionalMetric(metrics.hostDelayMs, "ms")}
        title={copy.dsp.diagnostics.hostDelayHelp}
      />
      <DiagnosticCell
        label={copy.dsp.diagnostics.rawSkew}
        value={formatOptionalMetric(metrics.hostDelayRawSkewMs, "ms", 1)}
        title={copy.dsp.diagnostics.rawSkewHelp}
      />
      <DiagnosticCell
        label={copy.dsp.diagnostics.hostJitter}
        value={formatOptionalMetric(metrics.hostDelayJitterMs, "ms", 1)}
        title={copy.dsp.diagnostics.hostJitterHelp}
      />
      <DiagnosticCell
        label={copy.dsp.diagnostics.calibration}
        value={calibrationLabel(metrics.hostDelayCalibrationState, copy)}
        title={copy.dsp.diagnostics.calibrationHelp}
      />
      <DiagnosticCell
        label={copy.dsp.diagnostics.delayResets}
        value={metrics.hostDelayResetEvents.toLocaleString(language)}
        title={copy.dsp.diagnostics.delayResetsHelp}
      />
      <DiagnosticCell
        label={copy.dsp.diagnostics.aecDelay}
        value={formatOptionalMetric(metrics.aecEstimatedDelayMs, "ms")}
        title={copy.dsp.diagnostics.aecDelayHelp}
      />
      <DiagnosticCell
        label={copy.dsp.diagnostics.erl}
        value={formatOptionalMetric(metrics.aecEchoReturnLossDb, "dB", 1)}
        title={copy.dsp.diagnostics.erlHelp}
      />
      <DiagnosticCell
        label={copy.dsp.diagnostics.erle}
        value={formatOptionalMetric(
          metrics.aecEchoReturnLossEnhancementDb,
          "dB",
          1,
        )}
        title={copy.dsp.diagnostics.erleHelp}
      />
      <DiagnosticCell
        label={copy.dsp.diagnostics.residualEcho}
        value={formatOptionalMetric(metrics.aecResidualEchoLikelihood, "", 2).trim()}
        title={copy.dsp.diagnostics.residualEchoHelp}
      />
      <DiagnosticCell
        label={copy.dsp.diagnostics.attenuation}
        value={formatOptionalMetric(metrics.aecAttenuationDb, "dB", 1)}
        title={copy.dsp.diagnostics.attenuationHelp}
      />
      <DiagnosticCell
        label={copy.dsp.diagnostics.inputPeak}
        value={formatOptionalMetric(metrics.inputPeakDbfs, "dBFS", 1)}
        title={copy.dsp.diagnostics.inputPeakHelp}
      />
      <DiagnosticCell
        label={copy.dsp.diagnostics.outputPeak}
        value={formatOptionalMetric(metrics.outputPeakDbfs, "dBFS", 1)}
        title={copy.dsp.diagnostics.outputPeakHelp}
      />
      <DiagnosticCell
        label={copy.dsp.diagnostics.inputClipping}
        value={(metrics.inputClippedSamples ?? 0).toLocaleString(language)}
        title={copy.dsp.diagnostics.inputClippingHelp}
      />
      <DiagnosticCell
        label={copy.dsp.diagnostics.limiterSamples}
        value={(metrics.limiterLimitedSamples ?? 0).toLocaleString(language)}
        title={copy.dsp.diagnostics.limiterSamplesHelp}
      />
      <DiagnosticCell
        label={copy.dsp.diagnostics.rnnoiseVad}
        value={formatOptionalMetric(metrics.vadProbability, "", 2).trim()}
        title={copy.dsp.diagnostics.rnnoiseVadHelp}
      />
      <DiagnosticCell
        label={copy.dsp.diagnostics.speechVad}
        value={booleanState(
          metrics.speechVadActive,
          copy.dsp.diagnostics.speech,
          copy.dsp.diagnostics.noise,
        )}
        title={copy.dsp.diagnostics.speechVadHelp}
      />
      <DiagnosticCell
        label={copy.dsp.diagnostics.gateState}
        value={booleanState(
          metrics.vadGateOpen,
          copy.dsp.diagnostics.open,
          copy.dsp.diagnostics.closed,
        )}
        title={copy.dsp.diagnostics.gateStateHelp}
      />
      <DiagnosticCell
        label={copy.dsp.diagnostics.gateEvents}
        value={(metrics.vadGateOpenEvents ?? 0).toLocaleString(language)}
        title={copy.dsp.diagnostics.gateEventsHelp}
      />
      <DiagnosticCell
        label={copy.dsp.diagnostics.echoGuardState}
        value={booleanState(
          metrics.echoGuardActive,
          copy.dsp.diagnostics.active,
          copy.dsp.diagnostics.idle,
        )}
        title={copy.dsp.diagnostics.echoGuardStateHelp}
      />
      <DiagnosticCell
        label={copy.dsp.diagnostics.echoGuardEvents}
        value={(metrics.echoGuardActiveEvents ?? 0).toLocaleString(language)}
        title={copy.dsp.diagnostics.echoGuardEventsHelp}
      />
      <DiagnosticCell
        label={copy.dsp.diagnostics.nearendCandidates}
        value={(metrics.echoGuardNearendCandidateEvents ?? 0).toLocaleString(language)}
        title={copy.dsp.diagnostics.nearendCandidatesHelp}
      />
      <DiagnosticCell
        label={copy.dsp.diagnostics.echoGuardReleases}
        value={(metrics.echoGuardReleaseEvents ?? 0).toLocaleString(language)}
        title={copy.dsp.diagnostics.echoGuardReleasesHelp}
      />
    </div>
  );
}

function DiagnosticCell({
  label,
  value,
  title,
}: {
  label: string;
  value: string;
  title: string;
}) {
  return (
    <div className="diagnostic-cell" title={title}>
      <span>{label}</span>
      <strong>{value}</strong>
    </div>
  );
}

function stageIcon(stage: DspStageId, size: number) {
  if (stage === "aec") return <Waves size={size} />;
  if (stage === "rnnoise") return <AudioLines size={size} />;
  if (stage === "agc2") return <Gauge size={size} />;
  if (stage === "vad") return <Mic2 size={size} />;
  return <ShieldCheck size={size} />;
}

function stageName(stage: DspStageId) {
  if (stage === "aec") return "AEC3";
  if (stage === "rnnoise") return "RNNoise";
  if (stage === "agc2") return "AGC2";
  if (stage === "vad") return "VAD Gate";
  return "Safety Limiter";
}

function stageSubtitle(stage: DspStageId, copy: Translation) {
  if (stage === "aec") return copy.dsp.aecSubtitle;
  if (stage === "rnnoise") return copy.dsp.rnnoiseSubtitle;
  if (stage === "agc2") return copy.dsp.agc2Subtitle;
  if (stage === "vad") return copy.dsp.vadSubtitle;
  return copy.dsp.limiterSubtitle;
}

function dspFeatureEnabled(
  settings: EngineSettings,
  feature: DspFeatureId,
) {
  if (feature === "aec") return settings.dsp.aec.enabled;
  if (feature === "rnnoise") return settings.dsp.rnnoise.enabled;
  if (feature === "agc2") return settings.dsp.agc2.enabled;
  return settings.dsp.vadGate.enabled;
}

function withDspFeatureEnabled(
  settings: EngineSettings,
  feature: DspFeatureId,
  enabled: boolean,
): EngineSettings {
  if (feature === "aec") {
    return {
      ...settings,
      dsp: {
        ...settings.dsp,
        aec: { ...settings.dsp.aec, enabled },
      },
    };
  }
  if (feature === "rnnoise") {
    return {
      ...settings,
      dsp: {
        ...settings.dsp,
        rnnoise: { ...settings.dsp.rnnoise, enabled },
      },
    };
  }
  if (feature === "agc2") {
    // Mirrors DspFeature::set_enabled in src-tauri/src/engine.rs, which arms the adaptive gain so
    // the quick toggle cannot leave AGC2 enabled but transparent.
    const armAdaptive =
      enabled &&
      !settings.dsp.agc2.adaptiveDigitalEnabled &&
      settings.dsp.agc2.fixedGainDb <= 0;
    return {
      ...settings,
      dsp: {
        ...settings.dsp,
        agc2: {
          ...settings.dsp.agc2,
          enabled,
          ...(armAdaptive ? { adaptiveDigitalEnabled: true } : {}),
        },
      },
    };
  }
  return {
    ...settings,
    dsp: {
      ...settings.dsp,
      vadGate: { ...settings.dsp.vadGate, enabled },
    },
  };
}

function quickDspStatus(
  enabled: boolean,
  bypassed: boolean,
  busy: boolean,
  copy: Translation,
) {
  if (busy) return copy.views.monitor.applying;
  if (bypassed) return copy.views.monitor.bypassed;
  return enabled
    ? copy.views.monitor.moduleEnabled
    : copy.views.monitor.moduleDisabled;
}

function booleanState(
  value: boolean | null | undefined,
  positive: string,
  negative: string,
) {
  if (value == null) return "--";
  return value ? positive : negative;
}

function calibrationLabel(
  state: DelayCalibrationState,
  copy: Translation,
) {
  if (state === "calibrated") return copy.dsp.diagnostics.calibrated;
  if (state === "warmingUp") return copy.dsp.diagnostics.warming;
  return copy.dsp.diagnostics.waiting;
}

function audioAnomalyTotal(metrics: EngineMetrics) {
  return (
    metrics.renderUnderruns +
    metrics.renderOverflows +
    metrics.captureOverflows +
    metrics.outputUnderruns +
    metrics.outputQueueOverflows +
    metrics.dspFailures
  );
}

function routeName(
  endpointId: string | undefined,
  endpoints: EndpointDescriptor[],
) {
  if (!endpointId) return "-";
  return (
    endpoints.find((endpoint) => endpoint.id === endpointId)?.friendlyName ??
    endpointId
  );
}

function describeConfigurationIssue(
  issue: ConfigurationIssue,
  endpoints: EndpointDescriptor[],
  messages: Translation["routing"],
) {
  if (issue.type === "physicalCaptureRequired") {
    return messages.physicalCaptureRequiredHelp(
      routeName(issue.virtualCaptureEndpointId ?? undefined, endpoints),
    );
  }
  if (issue.type === "virtualDeviceRequired") {
    if (issue.reason === "vbCableUnavailable") {
      return messages.vbCableMissingHelp;
    }
    return issue.reason === "customPairUnconfirmed"
      ? messages.customPairUnconfirmed
      : messages.customPairIncomplete;
  }
  const name = routeName(issue.endpointId, endpoints);
  if (issue.kind === "defaultRender") {
    return messages.defaultRenderConflict(name);
  }
  if (issue.kind === "fixedRender") return messages.renderConflict(name);
  return messages.physicalCaptureConflict(name);
}

function getConfigurationIssue(
  settings: EngineSettings,
  endpoints: EndpointDescriptor[],
): ConfigurationIssue | null {
  if (settings.virtualDeviceMode === "custom") {
    if (!settings.captureEndpointId) {
      return {
        type: "physicalCaptureRequired",
        virtualCaptureEndpointId: settings.virtualCaptureEndpointId,
      };
    }
    if (!settings.outputEndpointId || !settings.virtualCaptureEndpointId) {
      return {
        type: "virtualDeviceRequired",
        reason: "customPairIncomplete",
      };
    }
    if (!activeCustomPairIsTrusted(settings)) {
      return {
        type: "virtualDeviceRequired",
        reason: "customPairUnconfirmed",
      };
    }
    const physical = findEndpoint(
      endpoints,
      "capture",
      settings.captureEndpointId,
    );
    const virtualRender = findEndpoint(
      endpoints,
      "render",
      settings.outputEndpointId,
    );
    const virtualCapture = findEndpoint(
      endpoints,
      "capture",
      settings.virtualCaptureEndpointId,
    );
    if (
      physical &&
      (sameEndpoint(physical, virtualRender) ||
        sameEndpoint(physical, virtualCapture))
    ) {
      return {
        type: "physicalCaptureRequired",
        virtualCaptureEndpointId: physical.id,
      };
    }
    return virtualRender
      ? getVirtualRouteConflict(settings, endpoints, virtualRender)
      : null;
  }

  const pair = findStandardVbCablePair(endpoints);
  if (!pair) {
    return {
      type: "virtualDeviceRequired",
      reason: "vbCableUnavailable",
    };
  }
  const capture = settings.captureEndpointId
    ? findEndpoint(endpoints, "capture", settings.captureEndpointId)
    : endpoints.find(
        (endpoint) =>
          endpoint.flow === "capture" &&
          endpoint.defaultRoles.includes("multimedia"),
      );
  if (
    capture &&
    (sameEndpoint(capture, pair.capture) ||
      sameEndpoint(capture, pair.render) ||
      isStandardVbCableCapture(capture))
  ) {
    return {
      type: "physicalCaptureRequired",
      virtualCaptureEndpointId: capture.id,
    };
  }
  return getVirtualRouteConflict(settings, endpoints, pair.render);
}

function getVirtualRouteConflict(
  settings: EngineSettings,
  endpoints: EndpointDescriptor[],
  output: EndpointDescriptor,
): VirtualRouteConflictIssue | null {
  const render = settings.followDefaultRender
    ? endpoints.find(
        (endpoint) =>
          endpoint.flow === "render" &&
          endpoint.defaultRoles.includes("multimedia"),
      )
    : settings.renderEndpointId
      ? findEndpoint(endpoints, "render", settings.renderEndpointId)
      : undefined;
  if (!render || !sameEndpoint(render, output)) return null;
  return {
    type: "virtualRouteConflict",
    kind: settings.followDefaultRender ? "defaultRender" : "fixedRender",
    endpointId: render.id,
  };
}

function sameEndpointGroup(
  left: EndpointDescriptor | undefined,
  right: EndpointDescriptor | undefined,
) {
  return Boolean(left?.groupId && right?.groupId && left.groupId === right.groupId);
}

function sameEndpoint(
  left: EndpointDescriptor | undefined,
  right: EndpointDescriptor | undefined,
) {
  return Boolean(left && right && (left.id === right.id || sameEndpointGroup(left, right)));
}

function findEndpoint(
  endpoints: EndpointDescriptor[],
  flow: DataFlow,
  id: string,
) {
  return endpoints.find(
    (endpoint) => endpoint.flow === flow && endpoint.id === id,
  );
}

function findStandardVbCablePair(endpoints: EndpointDescriptor[]) {
  const renders = endpoints.filter(isStandardVbCableRender);
  const captures = endpoints.filter(isStandardVbCableCapture);
  for (const render of renders) {
    const capture = captures.find((candidate) =>
      sameEndpointGroup(render, candidate),
    );
    if (capture) return { render, capture };
  }
  if (renders.length === 1 && captures.length === 1) {
    return { render: renders[0], capture: captures[0] };
  }
  return null;
}

function isStandardVbCableRender(endpoint: EndpointDescriptor) {
  return (
    endpoint.flow === "render" &&
    standardVbName(endpoint.friendlyName, "cable input")
  );
}

function isStandardVbCableCapture(endpoint: EndpointDescriptor) {
  return (
    endpoint.flow === "capture" &&
    standardVbName(endpoint.friendlyName, "cable output")
  );
}

function standardVbName(name: string, endpointName: string) {
  const normalized = name.trim().toLowerCase();
  return (
    normalized === endpointName ||
    (normalized.startsWith(`${endpointName} (`) &&
      normalized.includes("vb-audio virtual cable"))
  );
}

function isConfiguredVirtualCapture(
  endpoint: EndpointDescriptor,
  settings: EngineSettings | null,
  endpoints: EndpointDescriptor[],
) {
  if (isStandardVbCableCapture(endpoint)) return true;
  if (!settings?.virtualCaptureEndpointId) return false;
  const configured = findEndpoint(
    endpoints,
    "capture",
    settings.virtualCaptureEndpointId,
  );
  return sameEndpoint(endpoint, configured);
}

function activeCustomPairIsTrusted(settings: EngineSettings) {
  if (
    settings.virtualDeviceMode !== "custom" ||
    !settings.outputEndpointId ||
    !settings.virtualCaptureEndpointId
  ) {
    return false;
  }
  return settings.trustedVirtualAudioPairs.some(
    (pair) =>
      pair.renderEndpointId === settings.outputEndpointId &&
      pair.captureEndpointId === settings.virtualCaptureEndpointId,
  );
}

function inferPairedCapture(
  render: EndpointDescriptor,
  captures: EndpointDescriptor[],
) {
  if (!render.groupId) return undefined;
  const matches = captures.filter(
    (capture) => capture.groupId && capture.groupId === render.groupId,
  );
  return matches.length === 1 ? matches[0] : undefined;
}

function buildDeviceDiagnostics(
  endpoints: EndpointDescriptor[],
  settings: EngineSettings,
  issue: ConfigurationIssue | null,
) {
  const vbCable = findStandardVbCablePair(endpoints);
  const lines = [
    "XMic audio device diagnostics",
    `VB-CABLE: ${vbCable ? "detected" : "not detected"}`,
    `Mode: ${settings.virtualDeviceMode ?? "automatic"}`,
    `Configuration issue: ${issue ? `${issue.type}:${"reason" in issue ? issue.reason : "kind" in issue ? issue.kind : "capture"}` : "none"}`,
    "Endpoints:",
  ];
  for (const endpoint of endpoints) {
    lines.push(
      `- [${endpoint.flow}] ${endpoint.friendlyName} | roles=${endpoint.defaultRoles.join(",") || "none"} | group=${endpoint.groupId ? "present" : "none"} | id#=${shortEndpointHash(endpoint.id)}`,
    );
  }
  return lines.join("\n");
}

function shortEndpointHash(value: string) {
  let hash = 0x811c9dc5;
  for (let index = 0; index < value.length; index += 1) {
    hash ^= value.charCodeAt(index);
    hash = Math.imul(hash, 0x01000193);
  }
  return (hash >>> 0).toString(16).padStart(8, "0");
}

export default App;
