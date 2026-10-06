import { describe, expect, it } from "vitest";
import {
  type ApplicationDiagnostics,
  buildRedactedApplicationDiagnostics,
  dspValidationState,
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
});
