import { describe, expect, it } from "vitest";
import { meterFillPercent } from "./levelMeter";

describe("meterFillPercent", () => {
  it("compresses low-level ambient noise while preserving travel for speech levels", () => {
    expect(meterFillPercent(-12, false)).toBe(0);
    expect(meterFillPercent(null, true)).toBe(0);
    expect(meterFillPercent(-60, true)).toBe(0);
    expect(meterFillPercent(-55, true)).toBe(0);
    // Quiet room floor (~-30 dBFS) stays under 20% instead of filling half the bar.
    expect(meterFillPercent(-30, true)).toBe(18);
    // Speech levels (-20 dBFS to 0 dBFS) span the upper ~63% of the meter.
    expect(meterFillPercent(-20, true)).toBe(37);
    expect(meterFillPercent(-12, true)).toBe(58);
    expect(meterFillPercent(-6, true)).toBe(78);
    expect(meterFillPercent(0, true)).toBe(100);
  });
});
