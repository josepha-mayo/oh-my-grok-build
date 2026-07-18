import { useEffect, useRef } from "react";
import { Html5QrcodeScanner } from "html5-qrcode";

interface QrScannerProps {
  onScan: (url: string) => void;
  onError?: (err: Error) => void;
}

export function QrScanner({ onScan, onError }: QrScannerProps) {
  const scannerRef = useRef<Html5QrcodeScanner | null>(null);
  const onScanRef = useRef(onScan);
  const onErrorRef = useRef(onError);

  useEffect(() => {
    onScanRef.current = onScan;
  }, [onScan]);

  useEffect(() => {
    onErrorRef.current = onError;
  }, [onError]);

  useEffect(() => {
    const scanner = new Html5QrcodeScanner(
      "qr-reader",
      { fps: 10, qrbox: { width: 250, height: 250 }, aspectRatio: 1 },
      false,
    );
    scannerRef.current = scanner;
    scanner.render(
      (decodedText) => {
        onScanRef.current(decodedText);
      },
      () => {
        // scan failures are expected while the camera is focusing
      },
    );

    return () => {
      void scanner.clear();
    };
  }, []);

  useEffect(() => {
    const timer = setTimeout(() => {
      const element = document.getElementById("qr-reader");
      if (
        !element ||
        element.innerText.includes("NotFoundError") ||
        element.innerText.includes("NotAllowedError")
      ) {
        onErrorRef.current?.(
          new Error(
            "Could not start camera. Make sure camera permission is allowed.",
          ),
        );
      }
    }, 3000);
    return () => clearTimeout(timer);
  }, []);

  return <div id="qr-reader" />;
}
