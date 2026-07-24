import { useEffect, useRef, useState } from "react";

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
  const [samples, setSamples] = useState<TelemetrySample[]>([]);

  useEffect(() => {
    if (!running || lastBlock.current === metrics.processedBlocks) return;
    lastBlock.current = metrics.processedBlocks;

    setSamples((current) => {
      const vad = Math.min(1, Math.max(0, metrics.vadProbability ?? 0));
      const attenuation = Math.min(
        1,
        Math.max(0, Math.abs(metrics.aecAttenuationDb ?? 0) / 36),
      );
      return [
        ...current,
        {
          block: metrics.processedBlocks,
          vad,
          attenuation,
          gateOpen: metrics.vadGateOpen ?? false,
        },
      ].slice(-SAMPLE_LIMIT);
    });
  }, [
    metrics.aecAttenuationDb,
    metrics.processedBlocks,
    metrics.vadGateOpen,
    metrics.vadProbability,
    running,
  ]);

  useEffect(() => {
    if (!running) {
      lastBlock.current = null;
      setSamples([]);
    }
  }, [running]);

  useEffect(() => {
    const canvas = canvasRef.current;
    if (!canvas) return;

    const render = () => {
      const bounds = canvas.getBoundingClientRect();
      const ratio = Math.min(window.devicePixelRatio || 1, 2);
      const width = Math.max(1, Math.round(bounds.width));
      const height = Math.max(1, Math.round(bounds.height));
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

      if (samples.length === 0) {
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
      samples.forEach((sample, index) => {
        if (!sample.gateOpen) return;
        context.fillStyle = COLORS.gate;
        context.fillRect(index * step - step / 2, 0, Math.max(2, step), height);
      });

      drawLine(context, samples, width, height, (sample) => sample.vad, COLORS.vad);
      drawLine(
        context,
        samples,
        width,
        height,
        (sample) => sample.attenuation,
        COLORS.attenuation,
      );

      const lastX = ((samples.length - 1) / (SAMPLE_LIMIT - 1)) * width;
      context.strokeStyle = COLORS.empty;
      context.beginPath();
      context.moveTo(lastX + 0.5, 0);
      context.lineTo(lastX + 0.5, height);
      context.stroke();
    };

    render();
    const observer = new ResizeObserver(render);
    observer.observe(canvas);
    return () => observer.disconnect();
  }, [samples]);

  return <canvas ref={canvasRef} className="telemetry-canvas" role="img" aria-label={label} />;
}
