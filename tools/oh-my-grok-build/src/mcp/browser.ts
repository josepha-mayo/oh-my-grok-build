import { lookup } from "node:dns/promises";
import { pathToFileURL } from "node:url";
import { startMcpServer, type McpContent, type McpTool } from "./runtime.js";
import { isAllowedHttpUrl, isPrivateIp } from "../net.js";

export function sanitizeAccessibilityRef(ref: string): string {
  const id = ref.replace(/^@/, "");
  if (!/^[A-Za-z0-9_-]+$/.test(id)) {
    throw new Error("Accessibility ref must be alphanumeric after '@'");
  }
  return `[data-accessibility-ref="${id}"]`;
}

type LookupFn = typeof lookup;

/**
 * Check a URL for SSRF safety. In addition to the static hostname checks in
 * isAllowedHttpUrl, we resolve the hostname and block any URL whose resolved IP
 * is private, link-local, or cloud metadata. This mitigates DNS-rebinding and
 * xip.io-style SSRF bypasses.
 */
export async function isUrlAllowed(
  raw: string,
  lookupFn?: LookupFn
): Promise<{ ok: true } | { ok: false; reason: string }> {
  const sync = isAllowedHttpUrl(raw);
  if (!sync.ok) return sync;
  try {
    const url = new URL(raw);
    const lookupImpl = lookupFn ?? lookup;
    const addresses = await lookupImpl(url.hostname, { all: true });
    if (!addresses.length) {
      return { ok: false, reason: `No DNS records for ${url.hostname}` };
    }
    for (const { address } of addresses) {
      if (isPrivateIp(address)) {
        return { ok: false, reason: `Blocked private IP address resolved from ${url.hostname}` };
      }
    }
  } catch (err) {
    const code = (err as { code?: string }).code;
    // Allow unresolvable hostnames to fail later in the browser. Anything else
    // (network / permission errors) is treated as a block to be safe.
    if (code !== "ENOTFOUND") {
      return { ok: false, reason: `DNS lookup failed: ${code ?? String(err)}` };
    }
  }
  return { ok: true };
}

let browser: any;
let context: any;
let page: any;
const routedPages = new WeakSet<any>();

function attachConsole(p: any): void {
  if (p.consoleLogs) return;
  p.consoleLogs = [];
  p.on("console", (msg: any) => {
    p.consoleLogs.push(`${msg.type()}: ${msg.text()}`);
  });
}

async function attachRoute(p: any): Promise<void> {
  if (routedPages.has(p)) return;
  routedPages.add(p);
  attachConsole(p);
  await p.route("**/*", async (route: any) => {
    const url = route.request().url();
    if (url.startsWith("data:") || url.startsWith("blob:")) {
      await route.continue();
      return;
    }
    const allowed = await isUrlAllowed(url);
    if (!allowed.ok) {
      await route.abort("blockedbyclient");
    } else {
      await route.continue();
    }
  });
}

async function ensurePage(): Promise<any> {
  if (page) return page;
  try {
    const { chromium } = await import("playwright-core");
    browser = await chromium.launch({ headless: true });
    context = await browser.newContext({ viewport: { width: 1280, height: 720 } });
    context.on("page", (p: any) => {
      void attachRoute(p);
    });
    page = await context.newPage();
    await attachRoute(page);
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
    const p = await ensurePage();
    try {
      await p.goto(url, { waitUntil: "networkidle" });
    } catch (err) {
      return textResult(`Could not navigate to ${url}: ${err instanceof Error ? err.message : String(err)}`);
    }
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
  description: "Return recent browser console logs captured since the last call.",
  inputSchema: { type: "object", properties: {} },
  async handler() {
    const p = await ensurePage();
    const logs = p.consoleLogs ?? [];
    p.consoleLogs = [];
    return textResult(logs.length ? logs.join("\n") : "No console logs captured.");
  },
};

const browserSetViewport: McpTool = {
  name: "browser_set_viewport",
  description: "Set the browser viewport size.",
  inputSchema: {
    type: "object",
    properties: {
      width: { type: "number", description: "Viewport width in pixels." },
      height: { type: "number", description: "Viewport height in pixels." },
    },
    required: ["width", "height"],
  },
  async handler(args) {
    const width = Number(args.width);
    const height = Number(args.height);
    if (!Number.isFinite(width) || !Number.isFinite(height) || width < 1 || height < 1) {
      throw new Error("width and height must be positive numbers");
    }
    const p = await ensurePage();
    await p.setViewportSize({ width, height });
    return textResult(`Viewport set to ${width}x${height}.`);
  },
};

const browserScroll: McpTool = {
  name: "browser_scroll",
  description: "Scroll the page by a relative amount or to a selector.",
  inputSchema: {
    type: "object",
    properties: {
      deltaY: { type: "number", description: "Vertical scroll delta in pixels (positive = down)." },
      ref: { type: "string", description: "Optional element ref or selector to scroll into view." },
    },
  },
  async handler(args) {
    const p = await ensurePage();
    const ref = String(args.ref ?? "").trim();
    if (ref) {
      const selector = ref.startsWith("@") ? sanitizeAccessibilityRef(ref) : ref;
      await p.evaluate(
        (sel: string) => document.querySelector(sel)?.scrollIntoView({ behavior: "auto", block: "center" }),
        selector
      );
    } else if (args.deltaY !== undefined) {
      const deltaY = Number(args.deltaY);
      if (!Number.isFinite(deltaY)) throw new Error("deltaY must be a number");
      await p.evaluate((dy: number) => window.scrollBy(0, dy), deltaY);
    } else {
      throw new Error("Provide ref or deltaY");
    }
    return textResult(`Scrolled.\n\n${await snapshot()}`);
  },
};

const browserGetUrl: McpTool = {
  name: "browser_get_url",
  description: "Return the current page URL and title.",
  inputSchema: { type: "object", properties: {} },
  async handler() {
    const p = await ensurePage();
    const url = p.url();
    const title = await p.title().catch(() => "");
    return textResult(`URL: ${url}\nTitle: ${title}`);
  },
};

const browserSelect: McpTool = {
  name: "browser_select",
  description: "Select an option in a <select> element.",
  inputSchema: {
    type: "object",
    properties: {
      ref: { type: "string", description: "Element ref or selector for the <select>." },
      value: { type: "string", description: "Option value to select." },
    },
    required: ["ref", "value"],
  },
  async handler(args) {
    const ref = String(args.ref ?? "").trim();
    const value = String(args.value ?? "");
    if (!ref) throw new Error("ref is required");
    const p = await ensurePage();
    const selector = ref.startsWith("@") ? sanitizeAccessibilityRef(ref) : ref;
    await p.selectOption(selector, value);
    return textResult(`Selected ${value} in ${ref}.\n\n${await snapshot()}`);
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

if (import.meta.url === pathToFileURL(process.argv[1] ?? "").href) {
  process.on("SIGINT", () => {
    void closeBrowser().then(
      () => process.exit(0),
      () => process.exit(1)
    );
  });
  process.on("SIGTERM", () => {
    void closeBrowser().then(
      () => process.exit(0),
      () => process.exit(1)
    );
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
      browserSetViewport,
      browserScroll,
      browserGetUrl,
      browserSelect,
      browserClose,
    ],
  });
}
