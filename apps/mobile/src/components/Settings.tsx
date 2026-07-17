import { useEffect, useState } from "react";
import { X, Trash2 } from "lucide-react";

const CONNECTIONS_KEY = "omgb:connections";
const PROVIDERS_KEY = "omgb:providers";

export interface Provider {
  id: string;
  name: string;
  model: string;
  baseUrl: string;
  apiKey: string;
  apiBackend: string;
}

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

function loadProviders(): Provider[] {
  try {
    const raw = localStorage.getItem(PROVIDERS_KEY);
    const parsed = raw ? (JSON.parse(raw) as unknown) : [];
    return Array.isArray(parsed) ? (parsed as Provider[]) : [];
  } catch {
    return [];
  }
}

function saveProviders(list: Provider[]) {
  try {
    localStorage.setItem(PROVIDERS_KEY, JSON.stringify(list));
  } catch {
    // ignore
  }
}

export function Settings({ onClose, onConnect, currentUrl }: SettingsProps) {
  const [tab, setTab] = useState<"connections" | "providers">("connections");
  const [connections, setConnections] = useState<SavedConnection[]>(loadConnections);
  const [providers, setProviders] = useState<Provider[]>(loadProviders);
  const [editing, setEditing] = useState<Provider | null>(null);
  const [form, setForm] = useState<Provider>({
    id: "",
    name: "",
    model: "",
    baseUrl: "",
    apiKey: "",
    apiBackend: "",
  });

  useEffect(() => saveConnections(connections), [connections]);
  useEffect(() => saveProviders(providers), [providers]);

  const updateForm = (field: keyof Provider, value: string) => {
    setForm((f) => ({ ...f, [field]: value }) as Provider);
  };

  const resetForm = () => {
    setForm({ id: "", name: "", model: "", baseUrl: "", apiKey: "", apiBackend: "" });
    setEditing(null);
  };

  const addProvider = () => {
    const p = { ...form, id: form.id || `provider-${Date.now()}` };
    setProviders((prev) => [...prev, p]);
    resetForm();
  };

  const updateProvider = () => {
    if (!editing) return;
    setProviders((prev) => prev.map((x) => (x.id === editing.id ? form : x)));
    resetForm();
  };

  const deleteProvider = (id: string) => {
    setProviders((prev) => prev.filter((x) => x.id !== id));
    if (editing?.id === id) resetForm();
  };

  const editProvider = (p: Provider) => {
    setEditing(p);
    setForm(p);
  };

  const addOllama = () => {
    setProviders((prev) => [
      ...prev,
      {
        id: `ollama-${Date.now()}`,
        name: "Ollama (local)",
        model: "llama3",
        baseUrl: "http://localhost:11434",
        apiKey: "",
        apiBackend: "ollama",
      },
    ]);
  };

  const addLmStudio = () => {
    setProviders((prev) => [
      ...prev,
      {
        id: `lmstudio-${Date.now()}`,
        name: "LM Studio (local)",
        model: "local",
        baseUrl: "http://localhost:1234/v1",
        apiKey: "",
        apiBackend: "openai",
      },
    ]);
  };

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

        <div className="settings-tabs">
          <button className={tab === "connections" ? "active" : ""} onClick={() => setTab("connections")}>
            Connections
          </button>
          <button className={tab === "providers" ? "active" : ""} onClick={() => setTab("providers")}>
            Providers
          </button>
        </div>

        {tab === "connections" ? (
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
        ) : (
          <div className="settings-form">
            <div className="one-tap-row">
              <button className="btn-secondary" onClick={addOllama}>
                + Ollama
              </button>
              <button className="btn-secondary" onClick={addLmStudio}>
                + LM Studio
              </button>
            </div>

            <div className="provider-fields">
              <input value={form.name} onChange={(e) => updateForm("name", e.target.value)} placeholder="Name" />
              <input value={form.model} onChange={(e) => updateForm("model", e.target.value)} placeholder="Model ID" />
              <input
                value={form.baseUrl}
                onChange={(e) => updateForm("baseUrl", e.target.value)}
                placeholder="Base URL"
              />
              <input
                value={form.apiBackend}
                onChange={(e) => updateForm("apiBackend", e.target.value)}
                placeholder="API backend (openai / ollama / ...)"
              />
              <input
                type="password"
                value={form.apiKey}
                onChange={(e) => updateForm("apiKey", e.target.value)}
                placeholder="API key (optional)"
              />
            </div>

            <div className="provider-actions">
              {editing ? (
                <>
                  <button className="btn-primary" onClick={updateProvider}>
                    Update
                  </button>
                  <button className="btn-secondary" onClick={resetForm}>
                    Cancel
                  </button>
                </>
              ) : (
                <button className="btn-primary" onClick={addProvider}>
                  Add provider
                </button>
              )}
            </div>

            <div className="settings-list providers-list">
              {providers.length === 0 ? (
                <p className="settings-empty">No providers.</p>
              ) : (
                providers.map((p) => (
                  <div key={p.id} className="settings-row" onClick={() => editProvider(p)}>
                    <div className="settings-row-fields">
                      <strong>{p.name}</strong>
                      <span className="settings-url">
                        {p.model} · {p.baseUrl}
                      </span>
                    </div>
                    <div className="settings-row-actions">
                      <button
                        className="icon-button"
                        onClick={(e) => {
                          e.stopPropagation();
                          deleteProvider(p.id);
                        }}
                      >
                        <Trash2 size={16} />
                      </button>
                    </div>
                  </div>
                ))
              )}
            </div>
          </div>
        )}
      </div>
    </div>
  );
}
