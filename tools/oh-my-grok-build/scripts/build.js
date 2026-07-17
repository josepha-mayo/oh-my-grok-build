#!/usr/bin/env node
import { execSync } from "child_process";
import { existsSync, readFileSync, writeFileSync } from "fs";
import { fileURLToPath } from "url";
import { dirname, join } from "path";

const __dirname = dirname(fileURLToPath(import.meta.url));
const dist = join(__dirname, "..", "dist");
const index = join(dist, "index.js");

execSync("npx tsc", { stdio: "inherit", cwd: join(__dirname, "..") });

if (existsSync(index)) {
  const content = readFileSync(index, "utf8");
  if (!content.startsWith("#!/usr/bin/env node")) {
    writeFileSync(index, `#!/usr/bin/env node\n${content}`);
  }
}
