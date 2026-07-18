import { mkdirSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import type { ChildProcess } from "node:child_process";
import chalk from "chalk";
import { getOmgDir, loadOmgConfig } from "../config.js";
import spawner from "../spawner.js";
import { isRateLimited, formatRateLimitMessage } from "../rate-limit.js";
import { appendTimelineEvent } from "../timeline.js";

export interface ResearchOptions {
  topic: string;
  count?: number;
  model?: string;
  yolo?: boolean;
}

interface ArxivEntry {
  id: string;
  title: string;
  summary: string;
  authors: string[];
  pdfUrl: string;
  htmlUrl: string;
}

function sanitizeFilename(name: string): string {
  const safe = name
    .replace(/[^a-zA-Z0-9_-]/g, "-")
    .replace(/-+/g, "-")
    .replace(/^-|-$/g, "");
  return safe || "research";
}

function xmlText(tag: string, block: string): string {
  const m = block.match(new RegExp(`<${tag}[^>]*>([\\s\\S]*?)</${tag}>`));
  return m
    ? m[1]
        .replace(/<[^>]+>/g, " ")
        .replace(/\s+/g, " ")
        .trim()
    : "";
}

function linkHrefByRel(block: string, rel: string): string {
  const re = new RegExp(
    `<link[^>]*?\\srel="${rel}"[^>]*?\\shref="([^"]+)"[^>]*>|<link[^>]*?\\shref="([^"]+)"[^>]*?\\srel="${rel}"[^>]*>`
  );
  const m = block.match(re);
  return m ? (m[1] ?? m[2] ?? "") : "";
}

async function searchArxiv(topic: string, rawCount: number): Promise<ArxivEntry[]> {
  const count = Number.isNaN(rawCount) ? 5 : Math.max(1, Math.min(20, rawCount));
  const url = `https://export.arxiv.org/api/query?search_query=all:${encodeURIComponent(topic)}&start=0&max_results=${count}&sortBy=relevance&sortOrder=descending`;
  const res = await fetch(url, { headers: { Accept: "application/atom+xml" } });
  if (res.status === 429) {
    throw new Error("arXiv rate limit reached. Please wait a moment and try again.");
  }
  if (!res.ok) throw new Error(`arXiv API error: ${res.status} ${res.statusText}`);
  const text = await res.text();

  const entries: ArxivEntry[] = [];
  const blocks = text.split("<entry").slice(1);
  for (const block of blocks) {
    const raw = "<entry" + block.split("</entry>")[0] + "</entry>";
    const id = xmlText("id", raw).replace(/^https?:\/\/arxiv.org\/abs\//, "");
    const title = xmlText("title", raw);
    const summary = xmlText("summary", raw);
    const authors = [...raw.matchAll(/<name>([\s\S]*?)<\/name>/g)].map((m) => m[1].trim());
    const htmlUrl = linkHrefByRel(raw, "alternate") || `https://arxiv.org/abs/${id}`;
    const pdfUrl = linkHrefByRel(raw, "related") || htmlUrl.replace("/abs/", "/pdf/");
    if (id && title) {
      entries.push({ id, title, summary, authors, pdfUrl, htmlUrl });
    }
  }
  return entries;
}

function buildPrompt(topic: string, entries: ArxivEntry[]): string {
  const abstracts = entries
    .map((e, i) => {
      const authors = e.authors.slice(0, 5).join(", ") + (e.authors.length > 5 ? " et al." : "");
      return `--- Paper ${i + 1} ---\nTitle: ${e.title}\nAuthors: ${authors}\nURL: ${e.htmlUrl}\nPDF: ${e.pdfUrl}\nSummary: ${e.summary.slice(0, 800)}`;
    })
    .join("\n\n");

  return [
    `You are a research assistant doing a deep dive on: "${topic}".`,
    "Below are recent arXiv abstracts. Write a research report that:",
    "1. Summarizes the state of the art and common themes across the papers.",
    "2. Identifies open problems, limitations, and research gaps.",
    "3. Proposes either a patch/improvement to an existing framework mentioned in the papers, or a novel idea if none fit.",
    "4. Include a concrete code snippet or patch in a fenced code block (e.g. ```python or ```diff) under a '## Proposed patch / implementation' section.",
    "",
    "Abstracts:",
    abstracts,
  ].join("\n");
}

function extractPatch(markdown: string): string | undefined {
  const section = markdown.match(/##\s*Proposed (?:patch|implementation)[^\n]*\n([\s\S]*?)(?:\n## |$)/i);
  if (section) {
    const block = section[1].match(/```[\s\S]*?```/);
    if (block) return block[0];
  }
  const fallback = markdown.match(/```[\s\S]*?```/);
  return fallback ? fallback[0] : undefined;
}

function runGrok(prompt: string, options: { model: string; yolo?: boolean }): Promise<string> {
  const args = ["-p", prompt, "--model", options.model];
  if (options.yolo) args.push("--yolo");

  return new Promise((resolve, reject) => {
    const chunks: Buffer[] = [];
    const proc = spawner.spawn("grok", args, {
      cwd: process.cwd(),
      stdio: ["ignore", "pipe", "pipe"],
      env: { ...process.env, GROK_DISABLE_AUTOUPDATER: "1" },
    }) as ChildProcess;
    proc.stdout?.on("data", (d: Buffer) => chunks.push(d));
    proc.stderr?.on("data", (d: Buffer) => chunks.push(d));
    proc.on("error", reject);
    proc.on("exit", (code) => {
      const output = Buffer.concat(chunks).toString("utf8");
      if (code === 0) {
        resolve(output);
      } else if (isRateLimited(output)) {
        reject(new Error(formatRateLimitMessage()));
      } else {
        reject(new Error(`grok exited with code ${code}\n${output}`));
      }
    });
  });
}

export async function researchCommand(options: ResearchOptions): Promise<void> {
  const cfg = await loadOmgConfig();
  const model = options.model ?? cfg.defaultModel ?? "grok-build";
  const count = Number.isNaN(options.count) ? 5 : Math.max(1, Math.min(20, options.count ?? 5));
  const topic = options.topic;

  appendTimelineEvent({ type: "research_start", topic, model, count });

  console.log(chalk.bold(`Searching arXiv for "${topic}"...`));
  const entries = await searchArxiv(topic, count);
  if (entries.length === 0) {
    throw new Error("No arXiv papers found for the given topic.");
  }
  console.log(chalk.green(`Found ${entries.length} paper(s).`));

  const prompt = buildPrompt(topic, entries);

  const dir = join(getOmgDir(), "research");
  mkdirSync(dir, { recursive: true });
  const base = join(dir, sanitizeFilename(topic));

  console.log(chalk.bold(`Running research synthesis with ${chalk.cyan(model)}...`));
  const report = await runGrok(prompt, { model, yolo: options.yolo });

  const reportPath = `${base}.md`;
  writeFileSync(reportPath, `# Research: ${topic}\n\n${report}`);
  console.log(chalk.dim(`Report saved to ${reportPath}`));

  const patch = extractPatch(report);
  if (patch) {
    const patchPath = `${base}.patch`;
    writeFileSync(patchPath, patch);
    console.log(chalk.dim(`Patch saved to ${patchPath}`));
  }

  appendTimelineEvent({ type: "research_stop", topic, model, papers: entries.length, reportPath, hasPatch: !!patch });
}
