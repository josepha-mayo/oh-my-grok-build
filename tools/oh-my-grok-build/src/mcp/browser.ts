import { hostname } from "node:os";
import { startMcpServer, type McpContent, type McpTool } from "./runtime.js";

const PRIVATE_HOSTS = new Set(["localhost", "127.0.0.1", "::1", "0.0.0.0"]);
const CLOUD_METADATA_HOSTS = new Set(["metadata.google.internal", "169.254.169.254"]);

function isPrivateIp(ip: string): boolean {
  if (ip.startsWith("::ffff:")) return isPrivateIp(ip.slice(7));
  if (ip === "127.0.0.1" || ip === "::1") return true;
  if (ip.startsWith("10.") || ip.startsWith("192.168.")) return true;
  if (/^172\.(1[6-9]|2[0-9]|3[0-1])\./.test(ip)) return true;
  if (ip.startsWith("169.254.")) return true;
  if (ip.startsWith("fc") || ip.startsWith("fd") || ip.startsWith("fe80:")) return true;
  return false;
}

function isAllowedUrl(raw: string): { ok: true } | { ok: false; reason: string } {
  let url: URL;
  try {
    url = new URL(raw);
  } catch {
    return { ok: false, reason: "Invalid URL" };
  }
  if (url.protocol !== "http:" && url.protocol !== "https:") {
    return { ok: false, reason: `Blocked non-HTTP(S) protocol: ${url.protocol}` };
  }
  const host = url.hostname.toLowerCase();
  if (PRIVATE_HOSTS.has(host) || CLOUD_METADATA_HOSTS.has(host)) {
    return { ok: false, reason: "Blocked local/private/metadata host" };
  }
  if (host === hostname().toLowerCase()) {
    return { ok: false, reason: "Blocked local machine hostname" };
  }
  if (isPrivateIp(host)) {
    return { ok: false, reason: "Blocked private IP address" };
  }
  if (url.username || url.password) {
    return { ok: false, reason: "URLs with embedded credentials are not allowed" };
  }
  return { ok: true };
}

function sanitizeAccessibilityRef(ref: string): string {
  const id = ref.replace(/^@/, "");
  if (!/^[A-Za-z0-9_-]+$/.test(id)) {
    throw new Error("Accessibility ref must be alphanumeric after '@'");
  }
  return `[data-accessibility-ref="${id}"]`;
}

let browser: any;
let context: any;
let page: any;

async function ensurePage(): Promise<any> {
  if (page) return page;
  try {
    const { chromium } = await import("playwright-core");
    browser = await chromium.launch({ headless: true });
    context = await browser.newContext({ viewport: { width: 1280, height: 720 } });
    page = await context.newPage();
    return page;
  } catch (err) {
    throw new Error(
      "Playwright is not installed or browsers are missing. Run: npm install -g playwright && npx playwright install chromium"
    );
  }
}

async function closeBrowser(): Promise<void> {
  if (page) {
    await page.close().catch(() => {});
    page = undefined;
  }
  if (context) {
    await context.close().catch(() => {});
    context = undefined;
  }
  if (browser) {
    await browser.close().catch(() => {});
    browser = undefined;
  }
}

function textResult(text: string): McpContent {
  return { type: "text", text } as const;
}

async function snapshot(): Promise<string> {
  const p = await ensurePage();
  const tree = await p.accessibility.snapshot();
  return tree ? JSON.stringify(tree, null, 2) : "No accessibility snapshot available.";
}

const browserNavigate: McpTool = {
  name: "browser_navigate",
  description: "Navigate the browser to a URL.",
  inputSchema: {
    type: "object",
    properties: { url: { type: "string", description: "URL to open." } },
    required: ["url"],
  },
  async handler(args) {
    const url = String(args.url ?? "").trim();
    if (!url) throw new Error("url is required");
    const allowed = isAllowedUrl(url);
    if (!allowed.ok) throw new Error(allowed.reason);
    const p = await ensurePage();
    await p.goto(url, { waitUntil: "networkidle" });
    return textResult(`Navigated to ${url}.\n\n${await snapshot()}`);
  },
};

const browserSnapshot: McpTool = {
  name: "browser_snapshot",
  description: "Return an accessibility snapshot of the current page.",
  inputSchema: { type: "object", properties: {} },
  async handler() {
    return textResult(await snapshot());
  },
};

const browserClick: McpTool = {
  name: "browser_click",
  description: "Click an element by its accessible ref or selector.",
  inputSchema: {
    type: "object",
    properties: {
      ref: { type: "string", description: "Element ref from snapshot (e.g. @e5) or CSS selector." },
    },
    required: ["ref"],
  },
  async handler(args) {
    const ref = String(args.ref ?? "").trim();
    if (!ref) throw new Error("ref is required");
    const p = await ensurePage();
    const selector = ref.startsWith("@") ? sanitizeAccessibilityRef(ref) : ref;
    await p.click(selector);
    return textResult(`Clicked ${ref}.\n\n${await snapshot()}`);
  },
};

const browserType: McpTool = {
  name: "browser_type",
  description: "Type text into a focused or targeted input.",
  inputSchema: {
    type: "object",
    properties: {
      ref: { type: "string", description: "Element ref or selector." },
      text: { type: "string", description: "Text to type." },
      submit: { type: "boolean", description: "Press Enter after typing." },
    },
    required: ["ref", "text"],
  },
  async handler(args) {
    const ref = String(args.ref ?? "").trim();
    const text = String(args.text ?? "");
    if (!ref) throw new Error("ref is required");
    const p = await ensurePage();
    const selector = ref.startsWith("@") ? sanitizeAccessibilityRef(ref) : ref;
    await p.fill(selector, text);
    if (args.submit) await p.press(selector, "Enter");
    return textResult(`Typed into ${ref}.\n\n${await snapshot()}`);
  },
};

const browserPress: McpTool = {
  name: "browser_press",
  description: "Press a keyboard key in the browser.",
  inputSchema: {
    type: "object",
    properties: { key: { type: "string", description: "Key to press (e.g. Enter, Tab)." } },
    required: ["key"],
  },
  async handler(args) {
    const key = String(args.key ?? "").trim();
    if (!key) throw new Error("key is required");
    const p = await ensurePage();
    await p.press("body", key);
    return textResult(`Pressed ${key}.\n\n${await snapshot()}`);
  },
};

const browserScreenshot: McpTool = {
  name: "browser_screenshot",
  description: "Take a screenshot of the current page.",
  inputSchema: { type: "object", properties: {} },
  async handler() {
    const p = await ensurePage();
    const buffer = await p.screenshot({ type: "png" });
    return { type: "image", data: (buffer as Buffer).toString("base64"), mimeType: "image/png" } as const;
  },
};

const browserConsole: McpTool = {
  name: "browser_console",
  description: "Return recent browser console logs.",
  inputSchema: { type: "object", properties: {} },
  async handler() {
    const p = await ensurePage();
    const logs: string[] = [];
    p.on("console", (msg: any) => logs.push(`${msg.type()}: ${msg.text()}`));
    return textResult(logs.length ? logs.join("\n") : "No console logs captured.");
  },
};

const browserClose: McpTool = {
  name: "browser_close",
  description: "Close the browser session.",
  inputSchema: { type: "object", properties: {} },
  async handler() {
    await closeBrowser();
    return textResult("Browser closed.");
  },
};

process.on("exit", () => void closeBrowser());
process.on("SIGINT", () => {
  void closeBrowser().then(() => process.exit(0));
});
process.on("SIGTERM", () => {
  void closeBrowser().then(() => process.exit(0));
});

startMcpServer({
  name: "omgb-browser",
  tools: [
    browserNavigate,
    browserSnapshot,
    browserClick,
    browserType,
    browserPress,
    browserScreenshot,
    browserConsole,
    browserClose,
  ],
});
