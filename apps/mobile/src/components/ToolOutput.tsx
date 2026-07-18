export interface ToolOutputData {
  terminal?: string;
  diff?: string | { before?: string; after?: string };
  image?: string;
  screenshot?: string;
  text?: string;
}

function normalizeOutput(raw: unknown): ToolOutputData | null {
  if (raw == null) return null;

  let parsed: unknown = raw;
  if (typeof raw === "string") {
    try {
      parsed = JSON.parse(raw);
    } catch {
      return { text: raw };
    }
  }

  if (typeof parsed !== "object" || parsed === null) {
    return { text: String(raw) };
  }

  const o = parsed as Record<string, unknown>;
  const out: ToolOutputData = {};

  if (typeof o.terminal === "string") out.terminal = o.terminal;
  if (typeof o.text === "string") out.text = o.text;
  if ("diff" in o) out.diff = o.diff as ToolOutputData["diff"];
  if (typeof o.image === "string") out.image = o.image;
  if (typeof o.screenshot === "string") out.screenshot = o.screenshot;

  if (!out.terminal && !out.diff && !out.image && !out.screenshot && !out.text) {
    out.text = JSON.stringify(o, null, 2);
  }

  return out;
}

function detectImageMime(base64: string): string | undefined {
  try {
    const prefixChars = Math.min(base64.length, 32) & ~3;
    if (prefixChars === 0) return undefined;
    const decoded = atob(base64.slice(0, prefixChars));
    const bytes = new Uint8Array(decoded.length);
    for (let i = 0; i < decoded.length; i++) {
      bytes[i] = decoded.charCodeAt(i);
    }
    if (bytes[0] === 0x89 && bytes[1] === 0x50 && bytes[2] === 0x4e && bytes[3] === 0x47) {
      return "image/png";
    }
    if (bytes[0] === 0xff && bytes[1] === 0xd8 && bytes[2] === 0xff) {
      return "image/jpeg";
    }
    if (bytes[0] === 0x47 && bytes[1] === 0x49 && bytes[2] === 0x46 && bytes[3] === 0x38) {
      return "image/gif";
    }
    if (
      bytes[0] === 0x52 &&
      bytes[1] === 0x49 &&
      bytes[2] === 0x46 &&
      bytes[3] === 0x46 &&
      bytes[8] === 0x57 &&
      bytes[9] === 0x45 &&
      bytes[10] === 0x42 &&
      bytes[11] === 0x50
    ) {
      return "image/webp";
    }
    const text = new TextDecoder().decode(bytes);
    const trimmed = text.trimStart().toLowerCase();
    if (trimmed.startsWith("<svg") || trimmed.startsWith("<?xml")) {
      return "image/svg+xml";
    }
  } catch {
    // not valid base64 or too short
  }
  return undefined;
}

function imageSrc(value: string): string {
  const trimmed = value.trim();
  if (/^(data:|https?:|\/\/)/i.test(trimmed)) return trimmed;

  const lower = trimmed.toLowerCase();
  if (lower.startsWith("<svg") || lower.startsWith("<?xml")) {
    return `data:image/svg+xml,${encodeURIComponent(trimmed)}`;
  }

  const base64 = value.replace(/\s/g, "");
  if (/^[A-Za-z0-9+/]+={0,2}$/.test(base64) && base64.length % 4 === 0) {
    const mime = detectImageMime(base64) ?? "image/png";
    return `data:${mime};base64,${base64}`;
  }

  return value;
}

function DiffView({ diff }: { diff: ToolOutputData["diff"] }) {
  if (typeof diff === "string") {
    return <pre className="tool-diff">{diff}</pre>;
  }
  if (diff && typeof diff === "object") {
    return (
      <div className="tool-diff-pair">
        <div>
          <span className="tool-diff-label">Before</span>
          <pre>{diff.before ?? ""}</pre>
        </div>
        <div>
          <span className="tool-diff-label">After</span>
          <pre>{diff.after ?? ""}</pre>
        </div>
      </div>
    );
  }
  return null;
}

export function ToolOutput({ output }: { output: ToolOutputData | unknown | undefined }) {
  const data = normalizeOutput(output);
  if (!data) return null;

  return (
    <div className="tool-output">
      {data.terminal ? <pre className="tool-terminal">{data.terminal}</pre> : null}
      {data.diff ? <DiffView diff={data.diff} /> : null}
      {data.image ? <img className="tool-image" src={imageSrc(data.image)} alt="" /> : null}
      {data.screenshot ? <img className="tool-image" src={imageSrc(data.screenshot)} alt="" /> : null}
      {data.text && !data.terminal && !data.diff && !data.image && !data.screenshot ? (
        <pre className="tool-terminal">{data.text}</pre>
      ) : null}
    </div>
  );
}
