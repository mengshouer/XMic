export type EngineState = "off" | "processing" | "degraded" | "blocked";
export type EngineOutput = "inactive" | "processed" | "raw" | "silence";
export type PrimaryReasonCode =
  | "manualOff"
  | "starting"
  | "aecWarmup"
  | "aecRebuild"
  | "waitingForPlayback"
  | "waitingForCapture"
  | "waitingForVirtualOutput"
  | "audioDeviceFailure"
  | "dspFailure"
  | "cpuOverload"
  | "safetyConflict"
  | "pairUnconfirmed"
  | "sessionUnavailable"
  | "helperUnavailable"
  | "crashLoop"
  | "settingsReset"
  | "workerTimeout";

export interface EngineUiStatus {
  desiredEnabled: boolean;
  state: EngineState;
  output: EngineOutput;
}

export interface OptionalDspState {
  aec: { enabled: boolean };
  rnnoise: { enabled: boolean };
  agc2: { enabled: boolean };
  vadGate: { enabled: boolean };
}

export function isOptionalDspBypass(dsp: OptionalDspState) {
  return (
    !dsp.aec.enabled &&
    !dsp.rnnoise.enabled &&
    !dsp.agc2.enabled &&
    !dsp.vadGate.enabled
  );
}

export function isEngineSessionActive(status: EngineUiStatus) {
  return (
    (status.state === "processing" || status.state === "degraded") &&
    status.output !== "inactive"
  );
}

export function resolveActiveCapture(
  activeCaptureEndpointId: string | null | undefined,
  appliedCaptureEndpointId: string | null,
  endpoints: readonly { id: string; friendlyName: string }[],
) {
  return {
    friendlyName: activeCaptureEndpointId
      ? (endpoints.find((endpoint) => endpoint.id === activeCaptureEndpointId)
          ?.friendlyName ?? null)
      : null,
    followsDefault:
      activeCaptureEndpointId != null && appliedCaptureEndpointId === null,
  };
}
