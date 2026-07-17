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

function imageSrc(value: string): string {
  if (value.startsWith("data:") || value.startsWith("http")) return value;
  return `data:image/png;base64,${value}`;
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
