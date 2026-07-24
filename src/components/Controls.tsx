import { ChevronDown, CircleHelp } from "lucide-react";
import {
  type CSSProperties,
  type ReactNode,
  useEffect,
  useId,
  useRef,
  useState,
} from "react";

export function HelpTip({ text }: { text: string }) {
  const tooltipId = useId();

  return (
    <span className="help-tip">
      <button
        className="help-tip-button"
        type="button"
        aria-label={text}
        aria-describedby={tooltipId}
      >
        <CircleHelp size={14} aria-hidden="true" />
      </button>
      <span className="help-tip-content" id={tooltipId} role="tooltip">
        {text}
      </span>
    </span>
  );
}

export function ControlLabel({
  label,
  help,
  htmlFor,
}: {
  label: string;
  help: string;
  htmlFor?: string;
}) {
  return (
    <span className="control-label">
      {htmlFor ? <label htmlFor={htmlFor}>{label}</label> : <span>{label}</span>}
      <HelpTip text={help} />
    </span>
  );
}

export function Toggle({
  label,
  help,
  checked,
  disabled,
  compact = false,
  onChange,
}: {
  label: string;
  help: string;
  checked: boolean;
  disabled?: boolean;
  compact?: boolean;
  onChange: (checked: boolean) => void;
}) {
  return (
    <div className={`toggle-control${compact ? " toggle-compact" : ""}`}>
      <label className="toggle-input">
        <input
          type="checkbox"
          checked={checked}
          disabled={disabled}
          onChange={(event) => onChange(event.target.checked)}
        />
        <span className="toggle-track" aria-hidden="true">
          <span />
        </span>
        <span className="toggle-label">{label}</span>
      </label>
      <HelpTip text={help} />
    </div>
  );
}

export function SelectControl({
  label,
  help,
  value,
  disabled,
  children,
  onChange,
}: {
  label: string;
  help: string;
  value: string;
  disabled?: boolean;
  children: ReactNode;
  onChange: (value: string) => void;
}) {
  const inputId = useId();

  return (
    <div className="select-control">
      <ControlLabel label={label} help={help} htmlFor={inputId} />
      <span className="select-input-wrap">
        <select
          id={inputId}
          value={value}
          disabled={disabled}
          onChange={(event) => onChange(event.target.value)}
        >
          {children}
        </select>
        <ChevronDown size={15} aria-hidden="true" />
      </span>
    </div>
  );
}

export function RangeControl({
  label,
  help,
  value,
  minimum,
  maximum,
  step,
  suffix,
  disabled,
  onChange,
}: {
  label: string;
  help: string;
  value: number;
  minimum: number;
  maximum: number;
  step: number;
  suffix: string;
  disabled?: boolean;
  onChange: (value: number) => void;
}) {
  const inputId = useId();
  const progress = ((value - minimum) / (maximum - minimum)) * 100;
  const rangeStyle = {
    "--range-progress": `${Math.min(100, Math.max(0, progress))}%`,
  } as CSSProperties;

  return (
    <div className="range-control">
      <ControlLabel label={label} help={help} htmlFor={inputId} />
      <input
        id={inputId}
        type="range"
        min={minimum}
        max={maximum}
        step={step}
        value={value}
        style={rangeStyle}
        disabled={disabled}
        onChange={(event) => onChange(Number(event.target.value))}
      />
      <output htmlFor={inputId}>
        {value}
        {suffix}
      </output>
    </div>
  );
}

function clamp(value: number, minimum: number, maximum: number) {
  return Math.min(maximum, Math.max(minimum, value));
}

function decimalPlaces(value: number) {
  const [, decimals = ""] = String(value).split(".");
  return decimals.length;
}

export function NumberControl({
  label,
  help,
  value,
  minimum,
  maximum,
  step,
  suffix,
  disabled,
  onChange,
}: {
  label: string;
  help: string;
  value: number;
  minimum: number;
  maximum: number;
  step: number;
  suffix: string;
  disabled?: boolean;
  onChange: (value: number) => void;
}) {
  const inputId = useId();
  const [inputValue, setInputValue] = useState(String(value));
  const editing = useRef(false);

  useEffect(() => {
    if (!editing.current) setInputValue(String(value));
  }, [value]);

  const commitValue = (candidate: string) => {
    const parsed = Number(candidate);
    if (candidate.trim() === "" || !Number.isFinite(parsed)) {
      setInputValue(String(value));
      return;
    }

    const nextValue = clamp(parsed, minimum, maximum);
    setInputValue(String(nextValue));
    if (nextValue !== value) onChange(nextValue);
  };

  const nudgeValue = (direction: 1 | -1) => {
    const parsed = Number(inputValue);
    const base = Number.isFinite(parsed) ? parsed : value;
    const precision = decimalPlaces(step);
    const nextValue = Number(
      clamp(base + direction * step, minimum, maximum).toFixed(precision),
    );
    setInputValue(String(nextValue));
    if (nextValue !== value) onChange(nextValue);
  };

  return (
    <div className="number-control">
      <ControlLabel label={label} help={help} htmlFor={inputId} />
      <span className="number-input-wrap">
        <input
          id={inputId}
          type="text"
          inputMode="decimal"
          role="spinbutton"
          aria-valuemin={minimum}
          aria-valuemax={maximum}
          aria-valuenow={value}
          value={inputValue}
          disabled={disabled}
          onFocus={() => {
            editing.current = true;
          }}
          onChange={(event) => {
            const nextInput = event.target.value;
            if (!/^-?(?:\d+(?:\.\d*)?|\.\d*)?$/.test(nextInput)) return;

            setInputValue(nextInput);
            const parsed = Number(nextInput);
            if (
              nextInput !== "" &&
              nextInput !== "-" &&
              nextInput !== "." &&
              nextInput !== "-." &&
              Number.isFinite(parsed) &&
              parsed >= minimum &&
              parsed <= maximum
            ) {
              onChange(parsed);
            }
          }}
          onBlur={() => {
            editing.current = false;
            commitValue(inputValue);
          }}
          onKeyDown={(event) => {
            if (event.key === "Enter") event.currentTarget.blur();
            if (event.key === "Escape") {
              setInputValue(String(value));
              event.currentTarget.blur();
            }
            if (event.key === "ArrowUp") {
              event.preventDefault();
              nudgeValue(1);
            }
            if (event.key === "ArrowDown") {
              event.preventDefault();
              nudgeValue(-1);
            }
          }}
        />
        <small>{suffix}</small>
      </span>
    </div>
  );
}
