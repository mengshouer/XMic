const METER_FLOOR_DBFS = -55;
const METER_CURVE_EXPONENT = 2.2;

export function meterFillPercent(
  peakDbfs: number | null | undefined,
  active: boolean,
): number {
  if (!active || peakDbfs == null || !Number.isFinite(peakDbfs)) return 0;
  // Map [-55 dBFS, 0 dBFS] with a power curve (t^2.2) so quiet room noise (~-30 dBFS) stays near
  // ~18% while normal speech (-20 dBFS to 0 dBFS) occupies the main upper range of the bar.
  const normalized = Math.min(
    1,
    Math.max(0, (peakDbfs - METER_FLOOR_DBFS) / -METER_FLOOR_DBFS),
  );
  return Math.round(normalized ** METER_CURVE_EXPONENT * 100);
}
