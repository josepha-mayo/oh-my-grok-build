import qrcode from "qrcode";
import chalk from "chalk";
import { startAgentServer, stopAgentServer } from "../acp/server.js";
import { loadOmgConfig } from "../config.js";
import type { ServeOptions } from "../acp/server.js";
import type { ServerInfo } from "../types.js";

export async function serveCommand(options: ServeOptions & { qr?: boolean }): Promise<ServerInfo> {
  const cfg = await loadOmgConfig();
  const model = options.model ?? cfg.defaultModel;

  console.log(chalk.dim("Starting Grok Build agent server..."));
  const server = await startAgentServer({
    bind: options.bind,
    port: options.port,
    secret: options.secret,
    cwd: options.cwd,
    model,
    yolo: options.yolo,
  });

  console.log(chalk.green("\nGrok agent server is running."));
  console.log(`  URL:    ${chalk.cyan(server.url)}`);
  console.log(`  Model:  ${chalk.cyan(model ?? "grok-build (default)")}`);
  console.log(`  CWD:    ${chalk.cyan(server.cwd)}`);

  if (options.qr !== false) {
    console.log("\nScan this QR code with the OMGB mobile app:");
    console.log(await qrcode.toString(server.url, { type: "terminal", small: true }));
  }

  console.log(chalk.dim("\nPress Ctrl+C to stop the server."));

  const onShutdown = () => {
    console.log(chalk.dim("\nShutting down agent server..."));
    void stopAgentServer(server)
      .catch(() => undefined)
      .finally(() => process.exit(0));
  };

  process.on("SIGINT", onShutdown);
  process.on("SIGTERM", onShutdown);

  return server;
}
