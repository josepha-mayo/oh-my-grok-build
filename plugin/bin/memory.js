#!/usr/bin/env node
const fs = require("fs");
const path = require("path");
const os = require("os");

const dir = path.join(os.homedir(), ".omgb");
const file = path.join(dir, "MEMORY.md");
const MAX_BULLETS = 50;

function readLines() {
  if (!fs.existsSync(file)) return ["# Agent Memory", ""];
  const raw = fs.readFileSync(file, "utf8");
  const lines = raw.split("\n");
  if (lines.length < 2 || !lines[0].startsWith("# ")) {
    return ["# Agent Memory", "", raw];
  }
  return lines;
}

function writeLines(lines) {
  fs.mkdirSync(dir, { recursive: true });
  fs.writeFileSync(file, lines.join("\n"));
}

function trimBullets(lines) {
  const bullets = lines.filter((l) => l.startsWith("- "));
  const nonBullets = lines.filter((l) => !l.startsWith("- "));
  if (bullets.length > MAX_BULLETS) {
    return [...nonBullets, ...bullets.slice(bullets.length - MAX_BULLETS)];
  }
  return lines;
}

function add(text) {
  const clean = text.replace(/\r?\n/g, " ").trim();
  if (!clean) {
    console.error("Usage: node memory.js add <text>");
    process.exit(1);
  }
  const lines = readLines();
  lines.push("- " + clean);
  writeLines(trimBullets(lines));
}

function list() {
  if (!fs.existsSync(file)) {
    console.log("# Agent Memory\n");
    return;
  }
  console.log(fs.readFileSync(file, "utf8"));
}

function trim() {
  if (!fs.existsSync(file)) return;
  writeLines(trimBullets(readLines()));
}

const [cmd, ...rest] = process.argv.slice(2);
const arg = rest.join(" ");

switch (cmd) {
  case "add":
    add(arg);
    break;
  case "list":
    list();
    break;
  case "trim":
    trim();
    break;
  default:
    console.error("Usage: node memory.js add <text>|list|trim");
    process.exit(1);
}
