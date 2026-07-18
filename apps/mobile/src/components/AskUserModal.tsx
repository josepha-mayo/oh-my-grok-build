import { useState } from "react";

interface AskUserModalProps {
  question: string;
  onSubmit: (value: string) => void;
  onCancel: () => void;
}

export function AskUserModal({ question, onSubmit, onCancel }: AskUserModalProps) {
  const [value, setValue] = useState("");

  return (
    <div className="modal-overlay" onClick={onCancel}>
      <div className="sheet" onClick={(e) => e.stopPropagation()}>
        <div className="sheet-header">
          <h3>{question}</h3>
        </div>
        <div className="ask-user-body">
          <input
            className="input"
            type="text"
            value={value}
            onChange={(e) => setValue(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter") {
                e.preventDefault();
                onSubmit(value);
              }
              if (e.key === "Escape") {
                e.preventDefault();
                onCancel();
              }
            }}
            autoFocus
            placeholder="Your answer..."
          />
          <div className="ask-user-actions">
            <button className="button secondary" onClick={onCancel}>
              Cancel
            </button>
            <button className="button primary" onClick={() => onSubmit(value)}>
              Submit
            </button>
          </div>
        </div>
      </div>
    </div>
  );
}
