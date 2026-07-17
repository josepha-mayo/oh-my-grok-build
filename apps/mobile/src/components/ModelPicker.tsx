import { X } from "lucide-react";

interface ModelPickerProps {
  models: string[];
  selected: string;
  onSelect: (model: string) => void;
  onClose: () => void;
}

const STARTER_MODELS = [
  "grok-build",
  "grok-2",
  "grok-2-vision",
  "claude-3-5-sonnet",
  "gpt-4o",
  "ollama",
];

export function ModelPicker({ models, selected, onSelect, onClose }: ModelPickerProps) {
  const list = Array.from(new Set([...STARTER_MODELS, ...models]));

  return (
    <div className="modal-overlay" onClick={onClose}>
      <div className="sheet" onClick={(e) => e.stopPropagation()}>
        <div className="sheet-header">
          <h3>Models</h3>
          <button className="icon-button" onClick={onClose}>
            <X size={20} />
          </button>
        </div>
        <div className="picker-list">
          {list.length === 0 ? (
            <div className="picker-empty">No models available.</div>
          ) : (
            list.map((m) => (
              <button
                key={m}
                className={`picker-item ${m === selected ? "selected" : ""}`}
                onClick={() => {
                  onSelect(m);
                  onClose();
                }}
              >
                <span>{m}</span>
                {m === selected ? <span className="picker-check">✓</span> : null}
              </button>
            ))
          )}
        </div>
      </div>
    </div>
  );
}
