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

export function buildRedactedApplicationDiagnostics(
  diagnostics: ApplicationDiagnostics,
  version: string | null,
): string {
  return [
    `XMic version: ${version ?? "unknown"}`,
    `Mode: ${diagnostics.mode}`,
    `DSP source: ${diagnostics.dspSource}`,
    `Custom DSP: ${diagnostics.dspCustom ? "yes" : "no"}`,
    `DSP library file: ${diagnostics.dspLibraryFileName || "unknown"}`,
    `Expected DSP ABI: ${diagnostics.expectedDspAbi}`,
    `Actual DSP ABI: ${diagnostics.actualDspAbi ?? "unavailable"}`,
    `DSP validation: ${dspValidationState(diagnostics)}`,
  ].join("\n");
}
