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
const COLORS = {
  background: "#101411",
  grid: "#273029",
  baseline: "#3a463d",
  vad: "#b7ff3c",
  attenuation: "#48d8d2",
  gate: "rgba(244, 184, 96, 0.16)",
  empty: "#6e7b72",
};

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
  running,
  label,
}: {
  metrics: TelemetryMetrics;
  running: boolean;
  label: string;
}) {
  const canvasRef = useRef<HTMLCanvasElement>(null);
  const lastBlock = useRef<number | null>(null);
  // A ref rather than state: the samples are only ever drawn onto the canvas, never rendered as JSX.
  // Holding them in state made every sample a React render, and re-ran the effect that owned the
  // ResizeObserver, so the observer was torn down and rebuilt once a second.
  const samples = useRef<TelemetrySample[]>([]);

  // Reads only refs, so it never changes identity and the observer below is installed once.
  const draw = useCallback(() => {
    const canvas = canvasRef.current;
    if (!canvas) return;
    const current = samples.current;
    const bounds = canvas.getBoundingClientRect();
    const ratio = Math.min(window.devicePixelRatio || 1, 2);
    const width = Math.max(1, Math.round(bounds.width));
    const height = Math.max(1, Math.round(bounds.height));
    // Assigning the size also clears the canvas and resets the transform below.
    canvas.width = Math.round(width * ratio);
    canvas.height = Math.round(height * ratio);

    const context = canvas.getContext("2d");
    if (!context) return;
    context.scale(ratio, ratio);
    context.fillStyle = COLORS.background;
    context.fillRect(0, 0, width, height);

    context.lineWidth = 1;
    context.strokeStyle = COLORS.grid;
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
      context.strokeStyle = COLORS.baseline;
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
      context.fillStyle = COLORS.gate;
      context.fillRect(index * step - step / 2, 0, Math.max(2, step), height);
    });

    drawLine(context, current, width, height, (sample) => sample.vad, COLORS.vad);
    drawLine(
      context,
      current,
      width,
      height,
      (sample) => sample.attenuation,
      COLORS.attenuation,
    );

    const lastX = ((current.length - 1) / (SAMPLE_LIMIT - 1)) * width;
    context.strokeStyle = COLORS.empty;
    context.beginPath();
    context.moveTo(lastX + 0.5, 0);
    context.lineTo(lastX + 0.5, height);
    context.stroke();
  }, []);

  // Installed once, because `draw` never changes identity.
  useEffect(() => {
    const canvas = canvasRef.current;
    if (!canvas) return;
    draw();
    const observer = new ResizeObserver(draw);
    observer.observe(canvas);
    return () => observer.disconnect();
  }, [draw]);

  useEffect(() => {
    if (!running || lastBlock.current === metrics.processedBlocks) return;
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
    running,
  ]);

  useEffect(() => {
    if (running) return;
    lastBlock.current = null;
    samples.current = [];
    draw();
  }, [draw, running]);

  return <canvas ref={canvasRef} className="telemetry-canvas" role="img" aria-label={label} />;
}
