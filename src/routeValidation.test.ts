import { describe, expect, it } from "vitest";
import rawCases from "../fixtures/route-validation-cases.json";
import {
  getConfigurationIssue,
  type ConfigurationIssue,
  type EndpointDescriptor,
  type RouteSettings,
} from "./routeValidation";

// The shared specification. The same vectors run against `configuration_issue` in
// src-tauri/src/route_validation.rs — that is the only thing keeping two implementations of the
// same safety rules from drifting apart. The settings in each vector are as they exist *after*
// `reconcile_virtual_device_settings`, because that is what this function sees at runtime: the
// draft it validates came from the backend, which had already reconciled it.
interface RouteValidationCase {
  name: string;
  why: string;
  settings: RouteSettings;
  endpoints: EndpointDescriptor[];
  expected: ConfigurationIssue | null;
}

// JSON imports have no literal types, so the shape has to be asserted rather than inferred.
const cases = rawCases as unknown as RouteValidationCase[];

describe("getConfigurationIssue", () => {
  it("runs against a non-empty fixture", () => {
    expect(cases.length).toBeGreaterThan(0);
  });

  it.each(cases)("$name", ({ settings, endpoints, expected, why }) => {
    expect(getConfigurationIssue(settings, endpoints), why).toEqual(expected);
  });
});
