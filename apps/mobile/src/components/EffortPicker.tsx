import { X } from "lucide-react";

export type ReasoningEffort = "low" | "medium" | "high" | "max";

interface EffortPickerProps {
  effort: ReasoningEffort;
  onSelect: (effort: ReasoningEffort) => void;
  onClose: () => void;
}

const EFFORTS: { value: ReasoningEffort; label: string; desc: string }[] = [
  { value: "low", label: "Low", desc: "Faster, simpler answers" },
  { value: "medium", label: "Medium", desc: "Balanced speed and depth" },
  { value: "high", label: "High", desc: "More thorough reasoning" },
  { value: "max", label: "Max", desc: "Deep, careful analysis" },
];

export function EffortPicker({ effort, onSelect, onClose }: EffortPickerProps) {
  return (
    <div className="modal-overlay" onClick={onClose}>
      <div className="sheet" onClick={(e) => e.stopPropagation()}>
        <div className="sheet-header">
          <h3>Reasoning effort</h3>
          <button className="icon-button" onClick={onClose}>
            <X size={20} />
          </button>
        </div>
        <div className="picker-list">
          {EFFORTS.map((e) => (
            <button
              key={e.value}
              className={`picker-item ${e.value === effort ? "selected" : ""}`}
              onClick={() => {
                onSelect(e.value);
                onClose();
              }}
            >
              <div>
                <div>{e.label}</div>
                <div className="settings-url">{e.desc}</div>
              </div>
              {e.value === effort ? <span className="picker-check">✓</span> : null}
            </button>
          ))}
        </div>
      </div>
    </div>
  );
}
