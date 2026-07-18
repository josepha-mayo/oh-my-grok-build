#!/usr/bin/env node
import { execSync } from "child_process";
import { existsSync, readFileSync, writeFileSync, mkdirSync, readdirSync, statSync, copyFileSync } from "fs";
import { fileURLToPath } from "url";
import { dirname, join, relative } from "path";

const __dirname = dirname(fileURLToPath(import.meta.url));
const root = join(__dirname, "..");
const src = join(root, "src");
const dist = join(root, "dist");
const index = join(dist, "index.js");

execSync("npx tsc", { stdio: "inherit", cwd: root });

function copyAssets(dir) {
  for (const entry of readdirSync(dir, { withFileTypes: true })) {
    const full = join(dir, entry.name);
    if (entry.isDirectory()) {
      copyAssets(full);
      continue;
    }
    if (entry.name.endsWith(".py")) {
      const out = join(dist, relative(src, full));
      mkdirSync(dirname(out), { recursive: true });
      copyFileSync(full, out);
    }
  }
}
copyAssets(src);

if (existsSync(index)) {
  const content = readFileSync(index, "utf8");
  if (!content.startsWith("#!/usr/bin/env node")) {
    writeFileSync(index, `#!/usr/bin/env node\n${content}`);
  }
}
