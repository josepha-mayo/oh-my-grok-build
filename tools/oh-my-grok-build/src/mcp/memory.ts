import { existsSync, readFileSync, writeFileSync } from "node:fs";
import { mkdirSync } from "node:fs";
import { join } from "node:path";
import { homedir } from "node:os";
import { startMcpServer, type McpTool } from "./runtime.js";

const MEMORY_FILE = join(homedir(), ".omgb", "memory.json");

interface MemoryEntry {
  id: string;
  content: string;
  tags: string[];
  createdAt: string;
  updatedAt: string;
}

interface MemoryStore {
  entries: MemoryEntry[];
}

function ensureStore(): MemoryStore {
  mkdirSync(join(homedir(), ".omgb"), { recursive: true });
  if (!existsSync(MEMORY_FILE)) return { entries: [] };
  try {
    return JSON.parse(readFileSync(MEMORY_FILE, "utf8")) as MemoryStore;
  } catch {
    return { entries: [] };
  }
}

function saveStore(store: MemoryStore): void {
  mkdirSync(join(homedir(), ".omgb"), { recursive: true });
  writeFileSync(MEMORY_FILE, JSON.stringify(store, null, 2), { mode: 0o600 });
}

function generateId(): string {
  return `${Date.now()}-${Math.random().toString(36).slice(2, 9)}`;
}

function rankEntries(query: string, entries: MemoryEntry[]): MemoryEntry[] {
  const q = query.toLowerCase();
  const words = q.split(/\s+/).filter(Boolean);
  return entries
    .map((e) => {
      let score = 0;
      const hay = `${e.content} ${e.tags.join(" ")}`.toLowerCase();
      if (hay.includes(q)) score += 10;
      for (const w of words) {
        if (hay.includes(w)) score += 1;
      }
      return { entry: e, score };
    })
    .filter((x) => x.score > 0)
    .sort((a, b) => b.score - a.score)
    .map((x) => x.entry);
}

const memoryRemember: McpTool = {
  name: "memory_remember",
  description: "Store a durable fact, preference, or observation for future sessions.",
  inputSchema: {
    type: "object",
    properties: {
      content: { type: "string", description: "The fact to remember." },
      tags: { type: "array", items: { type: "string" }, description: "Optional tags for categorization." },
    },
    required: ["content"],
  },
  async handler(args) {
    const content = String(args.content ?? "").trim();
    if (!content) throw new Error("content is required");
    const store = ensureStore();
    const entry: MemoryEntry = {
      id: generateId(),
      content,
      tags: Array.isArray(args.tags) ? args.tags.map(String) : [],
      createdAt: new Date().toISOString(),
      updatedAt: new Date().toISOString(),
    };
    store.entries.push(entry);
    saveStore(store);
    return { type: "text", text: `Remembered as ${entry.id}.` } as const;
  },
};

const memorySearch: McpTool = {
  name: "memory_search",
  description: "Search durable memory for relevant facts.",
  inputSchema: {
    type: "object",
    properties: {
      query: { type: "string", description: "Search query." },
      limit: { type: "number", description: "Maximum results to return." },
    },
    required: ["query"],
  },
  async handler(args) {
    const query = String(args.query ?? "").trim();
    if (!query) throw new Error("query is required");
    const store = ensureStore();
    const limit = typeof args.limit === "number" && args.limit > 0 ? args.limit : 5;
    const results = rankEntries(query, store.entries).slice(0, limit);
    if (results.length === 0) return { type: "text", text: "No matching memories found." } as const;
    const text = results.map((e, i) => `${i + 1}. ${e.content} (${e.tags.join(", ") || "no tags"})`).join("\n");
    return { type: "text", text } as const;
  },
};

const memoryForget: McpTool = {
  name: "memory_forget",
  description: "Remove a memory entry by its ID.",
  inputSchema: {
    type: "object",
    properties: {
      id: { type: "string", description: "The memory ID to remove." },
    },
    required: ["id"],
  },
  async handler(args) {
    const id = String(args.id ?? "").trim();
    if (!id) throw new Error("id is required");
    const store = ensureStore();
    const before = store.entries.length;
    store.entries = store.entries.filter((e) => e.id !== id);
    saveStore(store);
    return {
      type: "text",
      text: before === store.entries.length ? `No entry found for ${id}.` : `Forgot ${id}.`,
    } as const;
  },
};

const memoryStatus: McpTool = {
  name: "memory_status",
  description: "Report the number of stored memories.",
  inputSchema: { type: "object", properties: {} },
  async handler() {
    const store = ensureStore();
    return { type: "text", text: `${store.entries.length} memory entries stored.` } as const;
  },
};

startMcpServer({ name: "omgb-memory", tools: [memoryRemember, memorySearch, memoryForget, memoryStatus] });
