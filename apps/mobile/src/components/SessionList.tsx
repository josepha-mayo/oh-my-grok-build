import { X, Folder } from "lucide-react";

export interface SessionListItem {
  sessionId: string;
  title?: string;
  cwd?: string;
  updatedAt?: string;
}

interface SessionListProps {
  sessions: SessionListItem[];
  onSelect: (sessionId: string) => void;
  onClose: () => void;
}

export function SessionList({ sessions, onSelect, onClose }: SessionListProps) {
  return (
    <div className="modal-overlay" onClick={onClose}>
      <div className="sheet" onClick={(e) => e.stopPropagation()}>
        <div className="sheet-header">
          <h3>Sessions</h3>
          <button className="icon-button" onClick={onClose}>
            <X size={20} />
          </button>
        </div>
        <div className="picker-list">
          {sessions.length === 0 ? (
            <div className="picker-empty">No sessions found.</div>
          ) : (
            sessions.map((s) => (
              <button
                key={s.sessionId}
                className="picker-item"
                onClick={() => onSelect(s.sessionId)}
                style={{ alignItems: "flex-start", flexDirection: "column" }}
              >
                <div style={{ display: "flex", alignItems: "center", gap: "0.5rem" }}>
                  <Folder size={16} />
                  <span>{s.title || s.sessionId}</span>
                </div>
                {s.cwd ? <span className="settings-url">{s.cwd}</span> : null}
              </button>
            ))
          )}
        </div>
      </div>
    </div>
  );
}
