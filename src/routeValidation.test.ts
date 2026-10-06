import { describe, expect, it } from "vitest";
import rawCases from "../fixtures/route-validation-cases.json";
import {
  type ConfigurationIssue,
  type EndpointDescriptor,
  getConfigurationIssue,
  isConfiguredVirtualCapture,
  type RouteSettings,
} from "./routeValidation";

// The shared specification. The same vectors run against `configuration_issue` in
// src-tauri/src/route_validation.rs — that is the only thing keeping the two implementations of the
// same safety rules from drifting apart. The vectors use the current settings schema; first-run
// default selection is tested separately in the Rust route-validation module.
interface RouteValidationCase {
  name: string;
  why: string;
  settings: RouteSettings;
  endpoints: EndpointDescriptor[];
  expected: ConfigurationIssue | null;
}

// JSON imports have no literal types, so the shape has to be asserted rather than inferred.
const cases = rawCases as unknown as RouteValidationCase[];

function caseNamed(name: string) {
  const found = cases.find((candidate) => candidate.name === name);
  if (!found) throw new Error(`missing route-validation case: ${name}`);
  return found;
}

function endpointNamed(endpoints: EndpointDescriptor[], id: string) {
  const found = endpoints.find((endpoint) => endpoint.id === id);
  if (!found) throw new Error(`missing route-validation endpoint: ${id}`);
  return found;
}

describe("getConfigurationIssue", () => {
  it("runs against a non-empty fixture", () => {
    expect(cases.length).toBeGreaterThan(0);
  });

  it.each(cases)("$name", ({ settings, endpoints, expected, why }) => {
    expect(getConfigurationIssue(settings, endpoints), why).toEqual(expected);
  });

  it("rejects a different remaining VB-CABLE capture for a known offline pair", () => {
    const base = caseNamed(
      "a recognised VB-CABLE pair may be temporarily offline",
    );
    const endpoints: EndpointDescriptor[] = [
      {
        platform: "windows",
        id: "new-cable-output",
        groupId: null,
        friendlyName: "CABLE Output (VB-Audio Virtual Cable)",
        flow: "capture",
        defaultRoles: ["multimedia"],
      },
    ];

    expect(
      getConfigurationIssue(
        { ...base.settings, captureEndpointId: null },
        endpoints,
      ),
    ).toEqual({
      type: "physicalCaptureRequired",
      virtualCaptureEndpointId: "new-cable-output",
    });
  });

  it("lets custom mode follow a safe default physical microphone", () => {
    const base = caseNamed(
      "a custom virtual capture may be the Windows default and live in another container",
    );
    const endpoints = base.endpoints.map((endpoint) => ({
      ...endpoint,
      defaultRoles:
        endpoint.id === "microphone"
          ? (["multimedia"] as EndpointDescriptor["defaultRoles"])
          : [],
    }));

    expect(
      getConfigurationIssue(
        { ...base.settings, captureEndpointId: null },
        endpoints,
      ),
    ).toBeNull();
  });

  it("rejects a trusted custom capture by its remembered friendly name", () => {
    const base = caseNamed(
      "a custom virtual capture may be the Windows default and live in another container",
    );
    const settings = {
      ...base.settings,
      captureEndpointId: null,
    };
    const endpoints = base.endpoints.map((endpoint) =>
      endpoint.id === "virtual-capture"
        ? {
            ...endpoint,
            id: "reinstalled-capture",
            friendlyName: "custom virtual output",
            defaultRoles: ["multimedia"] as EndpointDescriptor["defaultRoles"],
          }
        : endpoint,
    );

    expect(getConfigurationIssue(settings, endpoints)).toEqual({
      type: "physicalCaptureRequired",
      virtualCaptureEndpointId: "reinstalled-capture",
    });
  });

  it("always filters a standard VB-CABLE capture after re-enumeration", () => {
    const capture: EndpointDescriptor = {
      platform: "windows",
      id: "re-enumerated-cable-output",
      groupId: null,
      friendlyName: "CABLE Output (VB-Audio Virtual Cable)",
      flow: "capture",
      defaultRoles: [],
    };
    const base = caseNamed("a renamed cable is not guessed to be VB-CABLE");

    expect(isConfiguredVirtualCapture(capture, base.settings, [capture])).toBe(
      true,
    );
  });

  it("filters a trusted custom capture by its remembered friendly name", () => {
    const base = caseNamed(
      "a custom virtual capture may be the Windows default and live in another container",
    );
    const renamed = {
      ...endpointNamed(base.endpoints, "virtual-capture"),
      id: "reinstalled-capture",
      friendlyName: "custom virtual output",
    };

    expect(isConfiguredVirtualCapture(renamed, base.settings, [renamed])).toBe(
      true,
    );
  });

  it("blocks a known cable ID that reappears under a nonstandard identity", () => {
    const base = caseNamed(
      "a recognised VB-CABLE pair may be temporarily offline",
    );
    const endpoints: EndpointDescriptor[] = [
      {
        platform: "windows",
        id: "cable-output",
        groupId: null,
        friendlyName: "Renamed virtual return",
        flow: "capture",
        defaultRoles: ["multimedia"],
      },
    ];

    expect(
      getConfigurationIssue(
        { ...base.settings, captureEndpointId: null },
        endpoints,
      ),
    ).toEqual({
      type: "virtualDeviceRequired",
      reason: "vbCableUnavailable",
    });
  });
});
