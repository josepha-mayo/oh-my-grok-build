import { runGrok, runGrokOnce } from "./grok.js";
import type { GrokEvent } from "./types.js";

interface TeamResult {
  agent: number;
  prompt: string;
  output: string;
  error?: string;
}

export async function runTeam(prompts: string[], opts: { cwd?: string; model?: string; yolo?: boolean; parallel?: boolean }): Promise<TeamResult[]> {
  if (opts.parallel === false) {
    const results: TeamResult[] = [];
    for (let i = 0; i < prompts.length; i++) {
      try {
        const out = await runGrokOnce(prompts[i], { cwd: opts.cwd, model: opts.model, yolo: opts.yolo });
        results.push({ agent: i, prompt: prompts[i], output: out });
      } catch (err) {
        results.push({ agent: i, prompt: prompts[i], output: "", error: (err as Error).message });
      }
    }
    return results;
  }

  return Promise.all(
    prompts.map((p, i) =>
      runGrokOnce(p, { cwd: opts.cwd, model: opts.model, yolo: opts.yolo })
        .then((out) => ({ agent: i, prompt: p, output: out }))
        .catch((err) => ({ agent: i, prompt: p, output: "", error: (err as Error).message }))
    )
  );
}

export function streamTeam(prompt: string, opts: { cwd?: string; model?: string; yolo?: boolean; agents?: number } = {}): AsyncGenerator<GrokEvent, void, unknown> {
  // Simplistic fan-out: spawn N grok processes, round-robin prompt to each and merge outputs.
  const n = Math.max(1, opts.agents ?? 1);
  return (async function* () {
    const results = await runTeam(Array(n).fill(prompt), { ...opts, parallel: true });
    for (const r of results) {
      yield { type: "text", data: `## Agent ${r.agent}\n\n${r.error ? `Error: ${r.error}` : r.output}\n\n` } as GrokEvent;
    }
    yield { type: "end" } as GrokEvent;
  })();
}
