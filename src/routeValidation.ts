// Route validation, mirrored from src-tauri/src/route_validation.rs.
//
// The frontend keeps its own copy so it can price an unapplied draft without a round trip to the
// helper. Both implementations are pinned to the same vectors in
// fixtures/route-validation-cases.json — see routeValidation.test.ts. Add a vector for a new rule.

export type DataFlow = "render" | "capture";
export type DeviceRole = "console" | "multimedia" | "communications";
export type VirtualDeviceMode = "vbCable" | "custom";
export type VirtualDeviceIssueReason =
  | "vbCableUnavailable"
  | "customPairIncomplete"
  | "customPairUnconfirmed";
export type VirtualRouteConflictKind = "defaultRender" | "fixedRender";

export interface PhysicalCaptureRequiredIssue {
  type: "physicalCaptureRequired";
  virtualCaptureEndpointId: string | null;
}

export interface VirtualDeviceRequiredIssue {
  type: "virtualDeviceRequired";
  reason: VirtualDeviceIssueReason;
}

export interface VirtualRouteConflictIssue {
  type: "virtualRouteConflict";
  kind: VirtualRouteConflictKind;
  endpointId: string;
}

export type ConfigurationIssue =
  | PhysicalCaptureRequiredIssue
  | VirtualDeviceRequiredIssue
  | VirtualRouteConflictIssue;

export interface EndpointDescriptor {
  platform: string;
  id: string;
  groupId: string | null;
  friendlyName: string;
  flow: DataFlow;
  defaultRoles: DeviceRole[];
}

export interface TrustedVirtualAudioPair {
  renderEndpointId: string;
  captureEndpointId: string;
  renderName: string;
  captureName: string;
}

export interface KnownVbCablePair {
  renderEndpointId: string;
  captureEndpointId: string;
}

/**
 * The settings surface route validation actually reads — `EngineSettings` satisfies it structurally.
 * It names exactly the fields the shared fixture has to spell out; the optional AEC flag keeps the
 * feedback-conflict rule aligned with runtime settings.
 */
export interface RouteSettings {
  followDefaultRender: boolean;
  renderEndpointId: string | null;
  captureEndpointId: string | null;
  outputEndpointId: string | null;
  virtualDeviceMode: VirtualDeviceMode | null;
  virtualCaptureEndpointId: string | null;
  trustedVirtualAudioPairs: TrustedVirtualAudioPair[];
  knownVbCablePair: KnownVbCablePair | null;
  dsp?: { aec: { enabled: boolean } };
}

export function getConfigurationIssue(
  settings: RouteSettings,
  endpoints: EndpointDescriptor[],
): ConfigurationIssue | null {
  if (settings.virtualDeviceMode === "custom") {
    if (!settings.outputEndpointId || !settings.virtualCaptureEndpointId) {
      return {
        type: "virtualDeviceRequired",
        reason: "customPairIncomplete",
      };
    }
    const trustedPair = activeCustomPair(settings);
    if (!trustedPair) {
      return {
        type: "virtualDeviceRequired",
        reason: "customPairUnconfirmed",
      };
    }
    const physical = selectedPhysicalCapture(settings, endpoints);
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
      (virtualRender &&
        !trustedNameMatches(trustedPair.renderName, virtualRender.friendlyName)) ||
      (virtualCapture &&
        !trustedNameMatches(trustedPair.captureName, virtualCapture.friendlyName))
    ) {
      return {
        type: "virtualDeviceRequired",
        reason: "customPairUnconfirmed",
      };
    }
    if (
      physical &&
      (isStandardVbCableCapture(physical) ||
        trustedVirtualCaptureNameMatches(settings, physical) ||
        sameEndpoint(physical, virtualRender) ||
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
    // Only a pair previously recognised from standard VB-CABLE endpoint names may be temporarily
    // absent. Arbitrary IDs remain a configuration block.
    if (knownVbCablePairIsOffline(settings)) {
      return knownOfflineVbCableSafetyIssue(settings, endpoints);
    }
    return {
      type: "virtualDeviceRequired",
      reason: "vbCableUnavailable",
    };
  }
  if (
    (settings.virtualDeviceMode === "vbCable" ||
      settings.outputEndpointId ||
      settings.virtualCaptureEndpointId) &&
    !configuredVbCablePairMatches(settings, pair)
  ) {
    return {
      type: "virtualDeviceRequired",
      reason: "vbCableUnavailable",
    };
  }
  const capture = selectedPhysicalCapture(settings, endpoints);
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

function configuredVbCablePairMatches(
  settings: RouteSettings,
  pair: { render: EndpointDescriptor; capture: EndpointDescriptor },
) {
  return (
    settings.outputEndpointId === pair.render.id &&
    settings.virtualCaptureEndpointId === pair.capture.id
  );
}

function knownVbCablePairIsOffline(settings: RouteSettings) {
  const known = settings.knownVbCablePair;
  return Boolean(
    known &&
      settings.virtualDeviceMode === "vbCable" &&
      settings.outputEndpointId === known.renderEndpointId &&
      settings.virtualCaptureEndpointId === known.captureEndpointId,
  );
}

function selectedPhysicalCapture(
  settings: RouteSettings,
  endpoints: EndpointDescriptor[],
) {
  return settings.captureEndpointId
    ? findEndpoint(endpoints, "capture", settings.captureEndpointId)
    : endpoints.find(
        (endpoint) =>
          endpoint.flow === "capture" &&
          endpoint.defaultRoles.includes("multimedia"),
      );
}

function knownOfflineVbCableSafetyIssue(
  settings: RouteSettings,
  endpoints: EndpointDescriptor[],
): ConfigurationIssue | null {
  if (knownVbCableIdentityWasReused(settings, endpoints)) {
    return {
      type: "virtualDeviceRequired",
      reason: "vbCableUnavailable",
    };
  }
  const physical = selectedPhysicalCapture(settings, endpoints);
  const virtualRender = settings.outputEndpointId
    ? findEndpoint(endpoints, "render", settings.outputEndpointId)
    : undefined;
  const virtualCapture = settings.virtualCaptureEndpointId
    ? findEndpoint(endpoints, "capture", settings.virtualCaptureEndpointId)
    : undefined;
  if (
    physical &&
    (isStandardVbCableCapture(physical) ||
      trustedVirtualCaptureNameMatches(settings, physical) ||
      sameEndpoint(physical, virtualRender) ||
      sameEndpoint(physical, virtualCapture) ||
      physical.id === settings.virtualCaptureEndpointId)
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

function knownVbCableIdentityWasReused(
  settings: RouteSettings,
  endpoints: EndpointDescriptor[],
) {
  const known = settings.knownVbCablePair;
  if (!known) return false;
  const render = endpoints.find(
    (endpoint) => endpoint.id === known.renderEndpointId,
  );
  const capture = endpoints.find(
    (endpoint) => endpoint.id === known.captureEndpointId,
  );
  return Boolean(
    (render && !isStandardVbCableRender(render)) ||
      (capture && !isStandardVbCableCapture(capture)),
  );
}

function getVirtualRouteConflict(
  settings: RouteSettings,
  endpoints: EndpointDescriptor[],
  output: EndpointDescriptor,
): VirtualRouteConflictIssue | null {
  if (settings.dsp?.aec.enabled === false) return null;
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

export function sameEndpointGroup(
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

export function findStandardVbCablePair(endpoints: EndpointDescriptor[]) {
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

export function isConfiguredVirtualCapture(
  endpoint: EndpointDescriptor,
  settings: RouteSettings | null,
  endpoints: EndpointDescriptor[],
) {
  if (isStandardVbCableCapture(endpoint)) return true;
  if (!settings?.virtualCaptureEndpointId) return false;
  if (trustedVirtualCaptureNameMatches(settings, endpoint)) return true;
  const configured = findEndpoint(
    endpoints,
    "capture",
    settings.virtualCaptureEndpointId,
  );
  return sameEndpoint(endpoint, configured);
}

export function trustedVirtualCaptureNameMatches(
  settings: RouteSettings,
  capture: EndpointDescriptor,
) {
  return Boolean(
    settings.virtualCaptureEndpointId &&
      settings.trustedVirtualAudioPairs.some(
        (pair) =>
          pair.captureEndpointId === settings.virtualCaptureEndpointId &&
          trustedNameMatches(pair.captureName, capture.friendlyName),
      ),
  );
}

function activeCustomPair(settings: RouteSettings) {
  if (
    settings.virtualDeviceMode !== "custom" ||
    !settings.outputEndpointId ||
    !settings.virtualCaptureEndpointId
  ) {
    return undefined;
  }
  return settings.trustedVirtualAudioPairs.find(
    (pair) =>
      pair.renderEndpointId === settings.outputEndpointId &&
      pair.captureEndpointId === settings.virtualCaptureEndpointId,
  );
}

function trustedNameMatches(expected: string, actual: string) {
  return (
    expected.trim().length > 0 &&
    expected.trim().toLowerCase() === actual.trim().toLowerCase()
  );
}

export function activeCustomPairIsTrusted(settings: RouteSettings) {
  return Boolean(activeCustomPair(settings));
}

export function inferPairedCapture(
  render: EndpointDescriptor,
  captures: EndpointDescriptor[],
) {
  if (!render.groupId) return undefined;
  const matches = captures.filter(
    (capture) => capture.groupId && capture.groupId === render.groupId,
  );
  return matches.length === 1 ? matches[0] : undefined;
}
