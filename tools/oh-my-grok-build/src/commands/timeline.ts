import chalk from "chalk";
import { readTimelineEvents } from "../timeline.js";

export interface TimelineOptions {
  count?: number;
  type?: string;
}

export function timelineCommand(options: TimelineOptions): void {
  const events = readTimelineEvents({ count: options.count ?? 50, type: options.type });

  if (events.length === 0) {
    console.log("No timeline events yet.");
    return;
  }

  console.log(chalk.bold("Recent timeline events:\n"));
  for (const e of events) {
    const ts = new Date(e.ts).toLocaleTimeString();
    const label = chalk.cyan(`[${e.type}]`);
    const summary = Object.entries(e)
      .filter(([k]) => k !== "ts" && k !== "type")
      .map(([k, v]) => `${k}=${JSON.stringify(v)}`)
      .join(" ")
      .slice(0, 120);
    console.log(`${chalk.dim(ts)} ${label} ${summary}`);
  }
}
