import chalk from "chalk";
import { killSubagent, listSubagents, spawnSubagent, subagentOutput } from "../subagents/engine.js";

export interface SubagentSpawnOptions {
  name: string;
  prompt: string;
  model?: string;
  yolo?: boolean;
  maxTurns?: number;
}

export async function subagentSpawnCommand(options: SubagentSpawnOptions): Promise<void> {
  const record = await spawnSubagent(options.name, options.prompt, {
    model: options.model,
    yolo: options.yolo,
    maxTurns: options.maxTurns,
  });
  console.log(chalk.bold(`Spawned subagent:`), chalk.cyan(record.name));
  console.log(chalk.dim(`  pid: ${record.pid}`));
  console.log(chalk.dim(`  worktree: ${record.worktree}`));
  console.log(chalk.dim(`  log: ${record.logPath}`));
}

export async function subagentListCommand(): Promise<void> {
  const agents = await listSubagents();
  if (agents.length === 0) {
    console.log(chalk.dim("No subagents."));
    return;
  }
  for (const agent of agents) {
    console.log(
      `${chalk.cyan(agent.name)} [${agent.running ? chalk.green("running") : chalk.gray("stopped")}] ${chalk.dim(agent.worktree)}`
    );
  }
}

export async function subagentKillCommand(name: string): Promise<void> {
  await killSubagent(name);
  console.log(chalk.bold(`Killed subagent:`), chalk.cyan(name));
}

export async function subagentLogsCommand(name: string, lines: number): Promise<void> {
  const output = await subagentOutput(name, lines);
  console.log(output || chalk.dim("(no log output)"));
}
