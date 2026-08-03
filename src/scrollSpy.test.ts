import { describe, expect, it } from "vitest";
import { activeViewAt, markerOffset } from "./scrollSpy";

// The scroll handler used to read `offsetTop` and `clientHeight` on every event, which forces a
// synchronous layout at frame rate. Moving the measurement to a ResizeObserver means the decision now
// runs on cached geometry, so these pin the three things that transcription could have got wrong.

describe("markerOffset", () => {
  it("sits 28% down a short viewport", () => {
    expect(markerOffset(500)).toBe(140);
  });

  it("stops descending on a tall viewport, so a short section can still win", () => {
    expect(markerOffset(2000)).toBe(180);
  });
});

describe("activeViewAt", () => {
  const tops = [0, 900, 1800];

  it("selects the first view before anything has passed the marker", () => {
    // The first section's top is 0, which is <= the marker even at rest — but the answer is the same
    // either way, and this is the state the sidebar starts in.
    expect(activeViewAt(0, 140, tops)).toBe("monitor");
  });

  it("selects the last section past the marker, not the first", () => {
    expect(activeViewAt(760, 140, tops)).toBe("routing");
    expect(activeViewAt(1660, 140, tops)).toBe("processing");
  });

  it("switches exactly when a section reaches the marker", () => {
    expect(activeViewAt(759, 140, tops)).toBe("monitor");
    expect(activeViewAt(760, 140, tops)).toBe("routing");
  });

  it("never matches a section that is missing or not measured yet", () => {
    // `[]` is what the ref holds before the first measurement, and a missing element is stored as
    // Infinity. Both have to leave the first view selected rather than throw or match.
    expect(activeViewAt(5000, 140, [])).toBe("monitor");
    expect(
      activeViewAt(5000, 140, [0, Number.POSITIVE_INFINITY, Number.POSITIVE_INFINITY]),
    ).toBe("monitor");
    expect(
      activeViewAt(5000, 140, [0, 900, Number.POSITIVE_INFINITY]),
    ).toBe("routing");
  });
});
