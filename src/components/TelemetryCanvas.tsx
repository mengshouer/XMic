import { useCallback, useEffect, useRef } from "react";

interface TelemetryMetrics {
  processedBlocks: number;
  vadProbability?: number | null;
  aecAttenuationDb?: number | null;
  vadGateOpen?: boolean | null;
}

interface TelemetrySample {
  block: number;
  vad: number;
  attenuation: number;
  gateOpen: boolean;
}

const SAMPLE_LIMIT = 72;
const DEFAULT_COLORS = {
  background: "#0b0f17",
  grid: "#233046",
  baseline: "#314361",
  vad: "#38bdf8",
  attenuation: "#34d399",
  gate: "rgba(251, 191, 36, 0.14)",
  empty: "#7e91ab",
};

function resolveThemeColors() {
  const styles = window.getComputedStyle(document.documentElement);
  return {
    background:
      styles.getPropertyValue("--canvas").trim() || DEFAULT_COLORS.background,
    grid: styles.getPropertyValue("--line").trim() || DEFAULT_COLORS.grid,
    baseline:
      styles.getPropertyValue("--line-strong").trim() ||
      DEFAULT_COLORS.baseline,
    vad: styles.getPropertyValue("--accent").trim() || DEFAULT_COLORS.vad,
    attenuation:
      styles.getPropertyValue("--cyan").trim() || DEFAULT_COLORS.attenuation,
    gate:
      styles.getPropertyValue("--gate-fill").trim() || DEFAULT_COLORS.gate,
    empty:
      styles.getPropertyValue("--text-muted").trim() || DEFAULT_COLORS.empty,
  };
}

function drawLine(
  context: CanvasRenderingContext2D,
  samples: TelemetrySample[],
  width: number,
  height: number,
  getValue: (sample: TelemetrySample) => number,
  color: string,
) {
  if (samples.length < 2) return;

  context.beginPath();
  samples.forEach((sample, index) => {
    const x = (index / (SAMPLE_LIMIT - 1)) * width;
    const y = height - getValue(sample) * height;
    if (index === 0) context.moveTo(x, y);
    else context.lineTo(x, y);
  });
  context.strokeStyle = color;
  context.lineWidth = 1.75;
  context.lineJoin = "round";
  context.lineCap = "round";
  context.stroke();
}

export function TelemetryCanvas({
  metrics,
  active,
  label,
}: {
  metrics: TelemetryMetrics;
  active: boolean;
  label: string;
}) {
  const canvasRef = useRef<HTMLCanvasElement>(null);
  const lastBlock = useRef<number | null>(null);
  const themeColors = useRef(DEFAULT_COLORS);
  // A ref rather than state: the samples are only ever drawn onto the canvas, never rendered as JSX.
  // Holding them in state made every sample a React render, and re-ran the effect that owned the
  // ResizeObserver, so the observer was torn down and rebuilt once a second.
  const samples = useRef<TelemetrySample[]>([]);

  // Reads only refs, so it never changes identity and the observer below is installed once.
  const draw = useCallback(() => {
    const canvas = canvasRef.current;
    if (!canvas) return;
    const colors = themeColors.current;
    const current = samples.current;
    const bounds = canvas.getBoundingClientRect();
    const ratio = Math.min(window.devicePixelRatio || 1, 2);
    const width = Math.max(1, Math.round(bounds.width));
    const height = Math.max(1, Math.round(bounds.height));
    const targetWidth = Math.round(width * ratio);
    const targetHeight = Math.round(height * ratio);
    if (canvas.width !== targetWidth || canvas.height !== targetHeight) {
      canvas.width = targetWidth;
      canvas.height = targetHeight;
    }

    const context = canvas.getContext("2d");
    if (!context) return;
    context.setTransform(ratio, 0, 0, ratio, 0, 0);
    context.fillStyle = colors.background;
    context.fillRect(0, 0, width, height);

    context.lineWidth = 1;
    context.strokeStyle = colors.grid;
    for (let row = 1; row < 4; row += 1) {
      const y = Math.round((height / 4) * row) + 0.5;
      context.beginPath();
      context.moveTo(0, y);
      context.lineTo(width, y);
      context.stroke();
    }
    for (let column = 1; column < 8; column += 1) {
      const x = Math.round((width / 8) * column) + 0.5;
      context.beginPath();
      context.moveTo(x, 0);
      context.lineTo(x, height);
      context.stroke();
    }

    if (current.length === 0) {
      context.strokeStyle = colors.baseline;
      context.setLineDash([5, 7]);
      context.beginPath();
      context.moveTo(0, height / 2 + 0.5);
      context.lineTo(width, height / 2 + 0.5);
      context.stroke();
      context.setLineDash([]);
      return;
    }

    const step = width / (SAMPLE_LIMIT - 1);
    current.forEach((sample, index) => {
      if (!sample.gateOpen) return;
      context.fillStyle = colors.gate;
      context.fillRect(index * step - step / 2, 0, Math.max(2, step), height);
    });

    drawLine(context, current, width, height, (sample) => sample.vad, colors.vad);
    drawLine(
      context,
      current,
      width,
      height,
      (sample) => sample.attenuation,
      colors.attenuation,
    );

    const lastX = ((current.length - 1) / (SAMPLE_LIMIT - 1)) * width;
    context.strokeStyle = colors.empty;
    context.beginPath();
    context.moveTo(lastX + 0.5, 0);
    context.lineTo(lastX + 0.5, height);
    context.stroke();
  }, []);

  // Installed once, because `draw` never changes identity.
  useEffect(() => {
    const canvas = canvasRef.current;
    if (!canvas) return;
    const syncThemeAndDraw = () => {
      themeColors.current = resolveThemeColors();
      draw();
    };
    syncThemeAndDraw();
    const observer = new ResizeObserver(draw);
    observer.observe(canvas);
    const themeObserver = new MutationObserver(syncThemeAndDraw);
    themeObserver.observe(document.documentElement, {
      attributes: true,
      attributeFilter: ["data-resolved-theme"],
    });
    return () => {
      observer.disconnect();
      themeObserver.disconnect();
    };
  }, [draw]);

  useEffect(() => {
    if (!active || lastBlock.current === metrics.processedBlocks) return;
    lastBlock.current = metrics.processedBlocks;
    samples.current = [
      ...samples.current,
      {
        block: metrics.processedBlocks,
        vad: Math.min(1, Math.max(0, metrics.vadProbability ?? 0)),
        attenuation: Math.min(
          1,
          Math.max(0, Math.abs(metrics.aecAttenuationDb ?? 0) / 36),
        ),
        gateOpen: metrics.vadGateOpen ?? false,
      },
    ].slice(-SAMPLE_LIMIT);
    draw();
  }, [
    draw,
    metrics.aecAttenuationDb,
    metrics.processedBlocks,
    metrics.vadGateOpen,
    metrics.vadProbability,
    active,
  ]);

  useEffect(() => {
    if (active) return;
    lastBlock.current = null;
    samples.current = [];
    draw();
  }, [active, draw]);

  return <canvas ref={canvasRef} className="telemetry-canvas" role="img" aria-label={label} />;
}
