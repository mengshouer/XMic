import { describe, expect, it } from "vitest";
import {
  type EngineUiStatus,
  isEngineSessionActive,
  isOptionalDspBypass,
  type PrimaryReasonCode,
  resolveActiveCapture,
} from "./engineStatus";

function status(
  overrides: Partial<EngineUiStatus> = {},
): EngineUiStatus {
  return {
    desiredEnabled: false,
    state: "off",
    output: "inactive",
    ...overrides,
  };
}

describe("EngineStatus UI contract", () => {
  it("keeps structured waiting and safety reason codes in the UI contract", () => {
    const reasons = [
      "waitingForPlayback",
      "waitingForCapture",
      "waitingForVirtualOutput",
      "audioDeviceFailure",
      "dspFailure",
      "safetyConflict",
      "pairUnconfirmed",
    ] as const satisfies readonly PrimaryReasonCode[];

    expect(reasons).toHaveLength(7);
  });

  it("labels the legal all-optional-off chain as bypass", () => {
    expect(
      isOptionalDspBypass({
        aec: { enabled: false },
        rnnoise: { enabled: false },
        agc2: { enabled: false },
        vadGate: { enabled: false },
      }),
    ).toBe(true);
    expect(
      isOptionalDspBypass({
        aec: { enabled: false },
        rnnoise: { enabled: true },
        agc2: { enabled: false },
        vadGate: { enabled: false },
      }),
    ).toBe(false);
  });

  it("resolves active capture from applied runtime state rather than draft state", () => {
    const resolved = resolveActiveCapture("active-mic", null, [
      { id: "active-mic", friendlyName: "Active microphone" },
      { id: "draft-mic", friendlyName: "Unapplied draft microphone" },
    ]);

    expect(resolved).toEqual({
      friendlyName: "Active microphone",
      followsDefault: true,
    });
  });

  it("keeps Degraded on while exposing its active fallback output", () => {
    const degraded = status({
      desiredEnabled: true,
      state: "degraded",
      output: "silence",
    });

    expect(degraded.desiredEnabled).toBe(true);
    expect(isEngineSessionActive(degraded)).toBe(true);
  });

  it("keeps Blocked on without treating its inactive output as a session", () => {
    const blocked = status({
      desiredEnabled: true,
      state: "blocked",
      output: "inactive",
    });

    expect(blocked.desiredEnabled).toBe(true);
    expect(isEngineSessionActive(blocked)).toBe(false);
  });

  it("keeps Off off with inactive output", () => {
    const off = status();

    expect(off.desiredEnabled).toBe(false);
    expect(isEngineSessionActive(off)).toBe(false);
  });
});
