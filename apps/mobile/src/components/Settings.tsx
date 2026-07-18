import { useEffect, useState } from "react";
import { X, Trash2 } from "lucide-react";

const CONNECTIONS_KEY = "omgb:connections";

interface SavedConnection {
  url: string;
  name: string;
}

interface SettingsProps {
  onClose: () => void;
  onConnect?: (url: string) => void;
  currentUrl?: string;
}

function loadConnections(): SavedConnection[] {
  try {
    const raw = localStorage.getItem(CONNECTIONS_KEY);
    const parsed = raw ? (JSON.parse(raw) as unknown) : [];
    return Array.isArray(parsed) ? (parsed as SavedConnection[]) : [];
  } catch {
    return [];
  }
}

function saveConnections(list: SavedConnection[]) {
  try {
    localStorage.setItem(CONNECTIONS_KEY, JSON.stringify(list));
  } catch {
    // ignore
  }
}

export function Settings({ onClose, onConnect, currentUrl }: SettingsProps) {
  const [connections, setConnections] = useState<SavedConnection[]>(loadConnections);

  useEffect(() => saveConnections(connections), [connections]);

  const updateConnectionName = (index: number, name: string) => {
    setConnections((prev) => {
      const next = [...prev];
      next[index] = { ...next[index], name };
      return next;
    });
  };

  const deleteConnection = (index: number) => {
    setConnections((prev) => prev.filter((_, i) => i !== index));
  };

  const connectTo = (url: string) => {
    onConnect?.(url);
    onClose();
  };

  return (
    <div className="modal-overlay" onClick={onClose}>
      <div className="sheet settings-sheet" onClick={(e) => e.stopPropagation()}>
        <div className="sheet-header">
          <h3>Settings</h3>
          <button className="icon-button" onClick={onClose}>
            <X size={20} />
          </button>
        </div>

        <p className="settings-empty" style={{ margin: "0 0 12px" }}>
          BYOK providers are managed on the paired machine with <code>omgb provider add</code>.
        </p>

        <div className="settings-list">
          {connections.length === 0 ? (
            <p className="settings-empty">No saved connections.</p>
          ) : (
            connections.map((c, i) => (
              <div key={c.url} className={`settings-row ${c.url === currentUrl ? "current" : ""}`}>
                <div className="settings-row-fields">
                  <input
                    value={c.name}
                    onChange={(e) => updateConnectionName(i, e.target.value)}
                    placeholder="Friendly name"
                  />
                  <span className="settings-url">{c.url}</span>
                </div>
                <div className="settings-row-actions">
                  {onConnect ? (
                    <button className="btn-small" onClick={() => connectTo(c.url)}>
                      Connect
                    </button>
                  ) : null}
                  <button className="icon-button" onClick={() => deleteConnection(i)}>
                    <Trash2 size={16} />
                  </button>
                </div>
              </div>
            ))
          )}
        </div>
      </div>
    </div>
  );
}
