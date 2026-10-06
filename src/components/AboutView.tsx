import { invoke } from "@tauri-apps/api/core";
import {
  BookOpenText,
  Check,
  Copy,
  ExternalLink,
  FileText,
  FolderCog,
  Info,
  RefreshCw,
  ShieldAlert,
  X,
} from "lucide-react";
import { useCallback, useEffect, useRef, useState } from "react";
import {
  type ApplicationDiagnostics,
  buildRedactedApplicationDiagnostics,
  dspValidationState,
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

export function AboutView({ active, version, copy }: AboutViewProps) {
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
        buildRedactedApplicationDiagnostics(diagnostics, version),
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

  const modeLabel = diagnostics ? copy.modes[diagnostics.mode] : "--";
  const sourceLabel = diagnostics ? copy.sources[diagnostics.dspSource] : "--";
  const validationState = diagnostics ? dspValidationState(diagnostics) : null;
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
              <strong>{copy.overviewTitle}</strong>
              <small>{copy.overviewSubtitle}</small>
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
              <strong>{copy.diagnosticsTitle}</strong>
              <small>{copy.diagnosticsSubtitle}</small>
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
            {diagnostics && (
              <dl className="about-diagnostics-list">
                <DiagnosticItem label={copy.mode} value={modeLabel} />
                <DiagnosticItem label={copy.dataDirectory} path={diagnostics.dataDirectory} />
                <DiagnosticItem
                  label={copy.runtimeDirectory}
                  path={diagnostics.runtimeDirectory}
                />
                <DiagnosticItem
                  label={copy.webviewDirectory}
                  path={diagnostics.webviewDataDirectory}
                />
                <DiagnosticItem label={copy.dspSource} value={sourceLabel} />
                <DiagnosticItem
                  label={copy.dspLibrary}
                  path={diagnostics.dspLibraryPath}
                />
                <DiagnosticItem
                  label={copy.expectedAbi}
                  value={String(diagnostics.expectedDspAbi)}
                />
                <DiagnosticItem
                  label={copy.actualAbi}
                  value={
                    diagnostics.actualDspAbi == null
                      ? copy.unavailable
                      : String(diagnostics.actualDspAbi)
                  }
                />
                <DiagnosticItem
                  label={copy.validation}
                  value={validationLabel}
                  detail={
                    diagnostics.dspValidationError ??
                    diagnostics.dspSelectionGuardError
                  }
                  danger={validationState === "failed"}
                />
              </dl>
            )}
          </div>
        </section>
      </div>

      <section className="surface about-notices">
        <header className="about-panel-heading">
          <span className="panel-icon" aria-hidden="true">
            <BookOpenText size={14} />
          </span>
          <span>
            <strong>{copy.noticesTitle}</strong>
            <small>{copy.noticesSubtitle}</small>
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
              <strong id="notice-dialog-title">
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
