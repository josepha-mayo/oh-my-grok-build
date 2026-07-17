import { useState, useEffect, useRef } from "react";
import { QrCode, Link2, Keyboard, Camera, ChevronRight } from "lucide-react";
import { Html5Qrcode } from "html5-qrcode";

export interface ConnectionScreenProps {
  defaultUrl?: string;
  onConnect: (url: string) => void;
}

export function ConnectionScreen({ defaultUrl = "", onConnect }: ConnectionScreenProps) {
  const [url, setUrl] = useState(defaultUrl);
  const [scanning, setScanning] = useState(false);
  const scannerRef = useRef<Html5Qrcode | null>(null);

  useEffect(() => {
    if (!scanning) return;

    const scanner = new Html5Qrcode("qr-reader");
    scannerRef.current = scanner;

    scanner
      .start(
        { facingMode: "environment" },
        { fps: 10, qrbox: { width: 250, height: 250 } },
        (decodedText) => {
          setUrl(decodedText);
          setScanning(false);
        },
        undefined
      )
      .catch(() => setScanning(false));

    return () => {
      if (scannerRef.current) {
        try {
          void Promise.resolve(scannerRef.current.stop()).catch(() => {});
          void Promise.resolve(scannerRef.current.clear()).catch(() => {});
        } catch {
          // ignore
        }
        scannerRef.current = null;
      }
    };
  }, [scanning]);

  const handleSubmit = (e: React.FormEvent) => {
    e.preventDefault();
    if (url.trim()) onConnect(url.trim());
  };

  return (
    <div className="connection-screen safe-area">
      <div className="connection-header">
        <h1>OMGB Mobile</h1>
        <p>Remote control for Grok Build</p>
      </div>

      {scanning ? (
        <div className="scanner">
          <div id="qr-reader" />
          <button className="btn-secondary" onClick={() => setScanning(false)}>
            Cancel scan
          </button>
        </div>
      ) : (
        <form onSubmit={handleSubmit} className="connection-form">
          <div className="input-group">
            <label>Pairing URL</label>
            <div className="url-input-wrap">
              <Link2 size={18} className="input-icon" />
              <input
                type="text"
                value={url}
                onChange={(e) => setUrl(e.target.value)}
                placeholder="ws://host:port/ws?server-key=..."
              />
            </div>
            <span className="hint">
              Run <code>omgb serve</code> on your laptop and scan the QR code.
            </span>
          </div>

          <div className="button-row">
            <button type="button" className="btn-secondary" onClick={() => setScanning(true)}>
              <Camera size={18} />
              Scan QR
            </button>
            <button type="submit" className="btn-primary" disabled={!url.trim()}>
              <ChevronRight size={18} />
              Connect
            </button>
          </div>

          <div className="manual-tip">
            <Keyboard size={14} />
            <span>You can also type the URL manually.</span>
          </div>
        </form>
      )}

      <div className="feature-list">
        <div className="feature">
          <QrCode size={20} />
          <span>Approve tools, view diffs, switch models.</span>
        </div>
      </div>
    </div>
  );
}
