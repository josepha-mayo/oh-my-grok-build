/// <reference types="vite/client" />

declare module "html5-qrcode" {
  export class Html5Qrcode {
    constructor(elementId: string);
    start(
      cameraId: string | { facingMode: string },
      config: {
        fps: number;
        qrbox?: { width: number; height: number } | number;
      },
      onScanSuccess: (decodedText: string) => void,
      onScanFailure?: () => void,
    ): Promise<void>;
    stop(): Promise<void>;
    clear(): Promise<void>;
  }

  export class Html5QrcodeScanner {
    constructor(
      elementId: string,
      config: {
        fps?: number;
        qrbox?: { width: number; height: number } | number;
        aspectRatio?: number;
      },
      verbose?: boolean,
    );
    render(
      onScanSuccess: (decodedText: string) => void,
      onScanFailure?: () => void,
    ): void;
    clear(): Promise<void>;
  }
}
