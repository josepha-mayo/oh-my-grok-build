#!/usr/bin/env node
const fs = require("fs");
const path = require("path");
const os = require("os");

const dir = path.join(os.homedir(), ".omgb");
const file = path.join(dir, "todo.json");

function load() {
  if (!fs.existsSync(file)) return [];
  try {
    return JSON.parse(fs.readFileSync(file, "utf8"));
  } catch {
    return [];
  }
}

function save(list) {
  fs.mkdirSync(dir, { recursive: true });
  fs.writeFileSync(file, JSON.stringify(list, null, 2));
}

function list() {
  const items = load();
  if (items.length === 0) {
    console.log("No tasks.");
    return;
  }
  for (const item of items) {
    const status = item.done ? "[x]" : "[ ]";
    console.log(`${status} ${item.id}: ${item.text}`);
  }
}

function add(text) {
  const clean = text.replace(/\r?\n/g, " ").trim();
  if (!clean) {
    console.error("Usage: node todo.js add <text>");
    process.exit(1);
  }
  const items = load();
  items.push({ id: Date.now(), text: clean, done: false });
  save(items);
  console.log(`Added task ${items[items.length - 1].id}.`);
}

function complete(id) {
  const items = load();
  const item = items.find((x) => String(x.id) === id);
  if (!item) {
    console.error(`Task ${id} not found.`);
    process.exit(1);
  }
  item.done = true;
  save(items);
  console.log(`Completed task ${id}.`);
}

function remove(id) {
  const items = load().filter((x) => String(x.id) !== id);
  save(items);
  console.log(`Removed task ${id}.`);
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
  case "done":
  case "complete":
    complete(arg);
    break;
  case "delete":
  case "remove":
    remove(arg);
    break;
  default:
    console.error("Usage: node todo.js add <text>|list|done <id>|delete <id>");
    process.exit(1);
}
