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
export type VirtualRouteConflictKind =
  | "defaultRender"
  | "fixedRender"
  | "physicalCapture";

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
  groupId?: string | null;
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

/**
 * The settings surface route validation actually reads — `EngineSettings` satisfies it structurally.
 * Keeping it narrow means the DSP settings do not have to live here, and it names exactly the fields
 * the shared fixture has to spell out.
 */
export interface RouteSettings {
  followDefaultRender: boolean;
  renderEndpointId: string | null;
  captureEndpointId: string | null;
  outputEndpointId: string | null;
  virtualDeviceMode: VirtualDeviceMode | null;
  virtualCaptureEndpointId: string | null;
  trustedVirtualAudioPairs: TrustedVirtualAudioPair[];
}

export function getConfigurationIssue(
  settings: RouteSettings,
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
  settings: RouteSettings,
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
  const configured = findEndpoint(
    endpoints,
    "capture",
    settings.virtualCaptureEndpointId,
  );
  return sameEndpoint(endpoint, configured);
}

export function activeCustomPairIsTrusted(settings: RouteSettings) {
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
