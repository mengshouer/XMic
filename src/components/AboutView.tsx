import { invoke } from "@tauri-apps/api/core";
import {
  Activity,
  BookOpenText,
  Check,
  Copy,
  ExternalLink,
  FileText,
  FolderCog,
  Info,
  RefreshCw,
  ShieldAlert,
  Trash2,
  X,
} from "lucide-react";
import { useCallback, useEffect, useRef, useState } from "react";
import {
  type ApplicationDiagnostics,
  buildRedactedApplicationDiagnostics,
  type DebugTraceState,
  dspValidationState,
  type EngineDiagnosticSnapshot,
  evaluateDebugFindings,
  mergeLiveDspValidation,
} from "../aboutDiagnostics";
import type { Translation } from "../i18n";
import {
  LICENSE_NOTICE_DEFINITIONS,
  type LicenseNoticeId,
  loadLicenseNotice,
} from "../licenseNotices";

interface AboutViewProps {
  active: boolean;
  version: string | null;
  copy: Translation["about"];
  engineStatus?: EngineDiagnosticSnapshot | null;
  debugTrace?: DebugTraceState | null;
  onToggleDebug?: () => void;
  onClearDebug?: () => void;
}

interface NoticeDialogState {
  id: LicenseNoticeId;
  content: string | null;
  error: string | null;
  loading: boolean;
}

type ExternalLinkTarget =
  | "project"
  | "mplLicense"
  | "mplSourceArchive";

export function AboutView({
  active,
  version,
  copy,
  engineStatus,
  debugTrace,
  onToggleDebug,
  onClearDebug,
}: AboutViewProps) {
  const [diagnostics, setDiagnostics] =
    useState<ApplicationDiagnostics | null>(null);
  const [diagnosticsLoading, setDiagnosticsLoading] = useState(false);
  const [diagnosticsError, setDiagnosticsError] = useState<string | null>(null);
  const [actionError, setActionError] = useState<string | null>(null);
  const [copied, setCopied] = useState(false);
  const [notice, setNotice] = useState<NoticeDialogState | null>(null);
  const diagnosticsRequested = useRef(false);
  const noticeRequest = useRef(0);
  const noticeDialog = useRef<HTMLDialogElement>(null);

  const requestDiagnostics = useCallback(async () => {
    diagnosticsRequested.current = true;
    setDiagnosticsLoading(true);
    setDiagnosticsError(null);
    try {
      setDiagnostics(
        await invoke<ApplicationDiagnostics>("get_application_diagnostics"),
      );
    } catch (reason) {
      setDiagnosticsError(String(reason));
    } finally {
      setDiagnosticsLoading(false);
    }
  }, []);

  useEffect(() => {
    if (active && !diagnosticsRequested.current) void requestDiagnostics();
  }, [active, requestDiagnostics]);

  const retryDiagnostics = () => {
    void requestDiagnostics();
  };

  const openExternalLink = async (target: ExternalLinkTarget) => {
    setActionError(null);
    try {
      await invoke("open_external_link", { target });
    } catch (reason) {
      setActionError(String(reason));
    }
  };

  const copyDiagnostics = async () => {
    if (!diagnostics) return;
    setActionError(null);
    try {
      await navigator.clipboard.writeText(
        buildRedactedApplicationDiagnostics(
          diagnostics,
          version,
          engineStatus,
          debugTrace,
        ),
      );
      setCopied(true);
      window.setTimeout(() => setCopied(false), 1800);
    } catch (reason) {
      setActionError(String(reason));
    }
  };

  const openNotice = (id: LicenseNoticeId) => {
    const request = ++noticeRequest.current;
    setNotice({ id, content: null, error: null, loading: true });
    window.setTimeout(() => {
      if (!noticeDialog.current?.open) noticeDialog.current?.showModal();
    }, 0);
    void loadLicenseNotice(id)
      .then((content) => {
        if (request === noticeRequest.current) {
          setNotice({ id, content, error: null, loading: false });
        }
      })
      .catch((reason) => {
        if (request === noticeRequest.current) {
          setNotice({ id, content: null, error: String(reason), loading: false });
        }
      });
  };

  const closeNotice = () => {
    noticeRequest.current += 1;
    noticeDialog.current?.close();
    setNotice(null);
  };

  const effectiveDiagnostics = diagnostics
    ? mergeLiveDspValidation(diagnostics, engineStatus)
    : null;
  const modeLabel = effectiveDiagnostics
    ? copy.modes[effectiveDiagnostics.mode]
    : "--";
  const sourceLabel = effectiveDiagnostics
    ? copy.sources[effectiveDiagnostics.dspSource]
    : "--";
  const validationState = effectiveDiagnostics
    ? dspValidationState(effectiveDiagnostics)
    : null;
  const validationLabels = {
    valid: copy.validationValid,
    failed: copy.validationFailed,
    pending: copy.validationPending,
  } as const;
  const validationLabel = validationState
    ? validationLabels[validationState]
    : "--";

  return (
    <div className="about-view">
      <div className="about-summary-grid">
        <section className="surface about-overview">
          <header className="about-panel-heading">
            <span className="panel-icon" aria-hidden="true">
              <Info size={14} />
            </span>
            <span>
              <strong title={copy.overviewTitle}>{copy.overviewTitle}</strong>
              <small title={copy.overviewSubtitle}>
                {copy.overviewSubtitle}
              </small>
            </span>
          </header>
          <div className="about-overview-body">
            <div className="about-product-row">
              <span className="brand-mark about-brand-mark" aria-hidden="true">
                <Info size={18} />
              </span>
              <span>
                <strong>XMic</strong>
                <small>
                  {copy.version}: {version ? `v${version}` : copy.unavailable}
                </small>
              </span>
              <span className="about-mode-badge">{modeLabel}</span>
            </div>
            {diagnostics?.dspCustom && (
              <div className="about-custom-warning" role="status">
                <ShieldAlert size={17} aria-hidden="true" />
                <span>
                  <strong>{copy.customDspTitle}</strong>
                  <small>{copy.customDspBody}</small>
                </span>
              </div>
            )}
            <div className="about-actions">
              <button
                type="button"
                onClick={() => openExternalLink("project")}
              >
                <ExternalLink size={14} aria-hidden="true" />
                {copy.openProject}
              </button>
              <button
                type="button"
                onClick={() => openExternalLink("mplLicense")}
              >
                <ExternalLink size={14} aria-hidden="true" />
                {copy.openMplLicense}
              </button>
              <button
                type="button"
                onClick={() => openExternalLink("mplSourceArchive")}
              >
                <ExternalLink size={14} aria-hidden="true" />
                {copy.openMplSource}
              </button>
            </div>
          </div>
        </section>

        <section className="surface about-diagnostics">
          <header className="about-panel-heading">
            <span className="panel-icon" aria-hidden="true">
              <FolderCog size={14} />
            </span>
            <span>
              <strong title={copy.diagnosticsTitle}>
                {copy.diagnosticsTitle}
              </strong>
              <small title={copy.diagnosticsSubtitle}>
                {copy.diagnosticsSubtitle}
              </small>
            </span>
            <button
              className="about-copy-button"
              type="button"
              onClick={copyDiagnostics}
              disabled={!diagnostics}
            >
              {copied ? (
                <Check size={13} aria-hidden="true" />
              ) : (
                <Copy size={13} aria-hidden="true" />
              )}
              {copied ? copy.copied : copy.copyDiagnostics}
            </button>
          </header>
          <div className="about-diagnostics-body" aria-live="polite">
            {diagnosticsLoading && (
              <p className="about-loading">{copy.diagnosticsLoading}</p>
            )}
            {diagnosticsError && !diagnosticsLoading && (
              <div className="about-inline-error" role="alert">
                <span>{copy.diagnosticsFailed}</span>
                <button type="button" onClick={retryDiagnostics}>
                  <RefreshCw size={13} aria-hidden="true" />
                  {copy.retry}
                </button>
              </div>
            )}
            {effectiveDiagnostics && (
              <dl className="about-diagnostics-list">
                <DiagnosticItem label={copy.mode} value={modeLabel} />
                <DiagnosticItem
                  label={copy.dataDirectory}
                  path={effectiveDiagnostics.dataDirectory}
                />
                <DiagnosticItem
                  label={copy.runtimeDirectory}
                  path={effectiveDiagnostics.runtimeDirectory}
                />
                <DiagnosticItem
                  label={copy.webviewDirectory}
                  path={effectiveDiagnostics.webviewDataDirectory}
                />
                <DiagnosticItem label={copy.dspSource} value={sourceLabel} />
                <DiagnosticItem
                  label={copy.dspLibrary}
                  path={effectiveDiagnostics.dspLibraryPath}
                />
                <DiagnosticItem
                  label={copy.expectedAbi}
                  value={String(effectiveDiagnostics.expectedDspAbi)}
                />
                <DiagnosticItem
                  label={copy.actualAbi}
                  value={
                    effectiveDiagnostics.actualDspAbi == null
                      ? copy.unavailable
                      : String(effectiveDiagnostics.actualDspAbi)
                  }
                />
                <DiagnosticItem
                  label={copy.validation}
                  value={validationLabel}
                  detail={
                    effectiveDiagnostics.dspValidationError ??
                    effectiveDiagnostics.dspSelectionGuardError
                  }
                  danger={validationState === "failed"}
                />
              </dl>
            )}
          </div>
        </section>
      </div>

      <section className="surface about-debug-panel">
        <header className="about-panel-heading">
          <span className="panel-icon" aria-hidden="true">
            <Activity size={14} />
          </span>
          <span>
            <strong title={copy.debug.title}>{copy.debug.title}</strong>
            <small title={copy.debug.subtitle}>{copy.debug.subtitle}</small>
          </span>
          <div className="about-debug-actions">
            <span
              className="about-mode-badge"
              data-recording={debugTrace?.enabled || undefined}
            >
              {debugTrace?.enabled
                ? `${copy.debug.recordingBadge} · ${debugTrace.samples.length}s`
                : copy.debug.idleBadge}
            </span>
            {onToggleDebug && (
              <button
                type="button"
                className="about-copy-button"
                aria-pressed={Boolean(debugTrace?.enabled)}
                onClick={onToggleDebug}
              >
                {debugTrace?.enabled ? copy.debug.disable : copy.debug.enable}
              </button>
            )}
            {onClearDebug && (debugTrace?.samples.length ?? 0) > 0 && (
              <button
                type="button"
                className="about-copy-button"
                onClick={onClearDebug}
              >
                <Trash2 size={13} aria-hidden="true" />
                {copy.debug.clear}
              </button>
            )}
            <button
              type="button"
              className="about-copy-button"
              onClick={copyDiagnostics}
              disabled={!diagnostics}
            >
              {copied ? (
                <Check size={13} aria-hidden="true" />
              ) : (
                <Copy size={13} aria-hidden="true" />
              )}
              {copied ? copy.copied : copy.debug.copyReport}
            </button>
          </div>
        </header>

        <div className="about-debug-body">
          {engineStatus && (
            <dl className="about-diagnostics-list about-debug-metrics-grid">
              <DiagnosticItem
                label={copy.debug.renderPeak}
                value={formatDb(engineStatus.metrics.renderPeakDbfs)}
              />
              <DiagnosticItem
                label={copy.debug.inputPeak}
                value={formatDb(engineStatus.metrics.inputPeakDbfs)}
              />
              <DiagnosticItem
                label={copy.debug.aecPeak}
                value={formatDb(engineStatus.metrics.aecOutputPeakDbfs)}
              />
              <DiagnosticItem
                label={copy.debug.outputPeak}
                value={formatDb(engineStatus.metrics.outputPeakDbfs)}
              />
              <DiagnosticItem
                label={copy.debug.renderStream}
                value={formatStreamSpec(
                  engineStatus.metrics.renderSampleRateHz,
                  engineStatus.metrics.renderChannels,
                  engineStatus.metrics.renderKeepaliveActive == null
                    ? null
                    : engineStatus.metrics.renderKeepaliveActive
                      ? copy.debug.keepaliveOn
                      : copy.debug.keepaliveOff,
                  copy.unavailable,
                )}
              />
              <DiagnosticItem
                label={copy.debug.captureStream}
                value={formatStreamSpec(
                  engineStatus.metrics.captureSampleRateHz,
                  engineStatus.metrics.captureChannels,
                  null,
                  copy.unavailable,
                )}
              />
              <DiagnosticItem
                label={copy.debug.clockDrift}
                value={`${formatPpm(engineStatus.metrics.renderDriftPpm)} / ${formatPpm(engineStatus.metrics.captureDriftPpm)}`}
              />
              <DiagnosticItem
                label={copy.debug.queueAndTimeouts}
                value={`Q ${engineStatus.metrics.renderQueueBlocks ?? 0}/${engineStatus.metrics.captureQueueBlocks ?? 0} · T ${engineStatus.metrics.renderWaitTimeouts ?? 0}/${engineStatus.metrics.captureWaitTimeouts ?? 0}`}
              />
            </dl>
          )}

          {debugTrace && debugTrace.samples.length >= 3 && (
            <div className="about-debug-findings">
              {evaluateDebugFindings(debugTrace.samples).map((code) => (
                <div
                  key={code}
                  className="about-custom-warning about-debug-finding"
                  data-healthy={code === "healthy" || undefined}
                  role="status"
                >
                  <Info size={15} aria-hidden="true" />
                  <span>{copy.debug.findings[code]}</span>
                </div>
              ))}
            </div>
          )}

          {debugTrace && debugTrace.samples.length > 0 ? (
            <pre className="about-debug-log">
              {formatDebugTimelinePreview(debugTrace)}
            </pre>
          ) : (
            <p className="about-debug-empty">{copy.debug.emptyHint}</p>
          )}
        </div>
      </section>

      <section className="surface about-notices">
        <header className="about-panel-heading">
          <span className="panel-icon" aria-hidden="true">
            <BookOpenText size={14} />
          </span>
          <span>
            <strong title={copy.noticesTitle}>{copy.noticesTitle}</strong>
            <small title={copy.noticesSubtitle}>{copy.noticesSubtitle}</small>
          </span>
        </header>
        <div className="about-notice-list">
          {LICENSE_NOTICE_DEFINITIONS.map((definition) => {
            const noticeCopy = copy.notices[definition.id];
            return (
              <button
                type="button"
                className="about-notice-button"
                key={definition.id}
                title={`${noticeCopy.title} · ${noticeCopy.description}`}
                onClick={() => openNotice(definition.id)}
              >
                <FileText size={15} aria-hidden="true" />
                <span>
                  <strong>{noticeCopy.title}</strong>
                  <small>{noticeCopy.description}</small>
                </span>
                <ExternalLink size={13} aria-hidden="true" />
              </button>
            );
          })}
        </div>
      </section>

      {actionError && (
        <p className="about-action-error" role="alert">
          {copy.actionFailed}: {actionError}
        </p>
      )}

      {notice != null && (
        <dialog
          className="notice-dialog"
          ref={noticeDialog}
          aria-labelledby="notice-dialog-title"
          onClose={() => {
            noticeRequest.current += 1;
            setNotice(null);
          }}
        >
          <header>
            <span>
              <FileText size={16} aria-hidden="true" />
              <strong
                id="notice-dialog-title"
                title={copy.notices[notice.id].title}
              >
                {copy.notices[notice.id].title}
              </strong>
            </span>
            <button
              type="button"
              className="icon-button"
              aria-label={copy.close}
              onClick={closeNotice}
            >
              <X size={15} aria-hidden="true" />
            </button>
          </header>
          <div className="notice-dialog-body" aria-live="polite">
            {notice.loading && (
              <p className="about-loading">{copy.noticeLoading}</p>
            )}
            {notice.error && (
              <div className="about-inline-error" role="alert">
                <span>{copy.noticeFailed}</span>
                <button type="button" onClick={() => openNotice(notice.id)}>
                  <RefreshCw size={13} aria-hidden="true" />
                  {copy.retry}
                </button>
              </div>
            )}
            {notice.content != null && <pre>{notice.content}</pre>}
          </div>
        </dialog>
      )}
    </div>
  );
}

function DiagnosticItem({
  label,
  value,
  path,
  detail,
  danger = false,
}: {
  label: string;
  value?: string;
  path?: string;
  detail?: string | null;
  danger?: boolean;
}) {
  return (
    <div className="about-diagnostic-item" data-danger={danger || undefined}>
      <dt>{label}</dt>
      <dd>{path ? <code>{path}</code> : value}</dd>
      {detail && <small>{detail}</small>}
    </div>
  );
}

function formatDb(value: number | null | undefined): string {
  if (value == null || !Number.isFinite(value)) return "--";
  return `${value.toFixed(1)} dBFS`;
}

function formatPpm(value: number | null | undefined): string {
  if (value == null || !Number.isFinite(value)) return "--";
  return `${value >= 0 ? "+" : ""}${value.toFixed(1)} ppm`;
}

function formatStreamSpec(
  sampleRateHz: number | null | undefined,
  channels: number | null | undefined,
  extra: string | null,
  fallback: string,
): string {
  if (sampleRateHz == null) return fallback;
  const base = `${sampleRateHz} Hz · ${channels ?? "?"}ch`;
  return extra ? `${base} · ${extra}` : base;
}

function formatDebugTimelinePreview(trace: DebugTraceState): string {
  const lines: string[] = [];
  if (trace.events.length > 0) {
    lines.push("Events:");
    for (const event of trace.events.slice(-12)) {
      lines.push(`  [+${event.elapsedSec}s #${event.seq}] ${event.summary}`);
    }
    lines.push("");
  }
  lines.push(
    "t     | state/out              | R / In / AEC / Out dBFS     | H/Skew/AEC ms | ERL / ERLE / Res  | VAD / Gate",
  );
  for (const sample of trace.samples.slice(-30)) {
    const t = `+${String(sample.elapsedSec).padStart(3, " ")}s`;
    const state = `${sample.state}/${sample.output}${sample.reasonCode ? `(${sample.reasonCode})` : ""}`.padEnd(22, " ");
    const fmt = (n: number | null, d = 1) =>
      n == null || !Number.isFinite(n) ? "  -- " : n.toFixed(d).padStart(5, " ");
    const levels = `${fmt(sample.renderPeakDbfs)}/${fmt(sample.inputPeakDbfs)}/${fmt(sample.aecOutputPeakDbfs)}/${fmt(sample.outputPeakDbfs)}`;
    const delays = `${fmt(sample.hostDelayMs, 0)}/${fmt(sample.rawSkewMs, 1)}/${fmt(sample.aecEstimatedDelayMs, 0)}`;
    const echo = `${fmt(sample.erlDb, 1)}/${fmt(sample.erleDb, 1)}/${fmt(sample.residualEchoLikelihood, 2)}`;
    const gate = `${fmt(sample.vadProbability, 2)} ${sample.vadGateOpen == null ? "-" : sample.vadGateOpen ? "open" : "shut"}`;
    lines.push(`${t} | ${state} | ${levels} | ${delays} | ${echo} | ${gate}`);
  }
  return lines.join("\n");
}
