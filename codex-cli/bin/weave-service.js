#!/usr/bin/env node
// Start/stop the Weave coordinator as a background process (macOS only).

import { spawn } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";

const __filename = fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);

const command = process.argv[2];
if (!command || (command !== "start" && command !== "stop")) {
  // eslint-disable-next-line no-console
  console.error("Usage: weave-service <start|stop>");
  process.exit(1);
}

if (process.platform !== "darwin") {
  // eslint-disable-next-line no-console
  console.error("weave-service is only supported on macOS.");
  process.exit(1);
}

const weaveHome = resolveWeaveHome();
const pidPath = path.join(weaveHome, "weave-service.pid");
const logPath = path.join(weaveHome, "weave-service.log");

const binaryPath = resolveWeaveBinaryPath();
if (!binaryPath) {
  // eslint-disable-next-line no-console
  console.error(
    "Weave binary not found. Expected vendor/weave/<platform>/weave or repo root ./weave.",
  );
  process.exit(1);
}

if (command === "start") {
  startService();
} else {
  await stopService();
}

function resolveWeaveHome() {
  const envValue = process.env.WEAVE_HOME;
  if (envValue !== undefined) {
    const trimmed = envValue.trim();
    if (!trimmed) {
      // eslint-disable-next-line no-console
      console.error("WEAVE_HOME is set but empty.");
      process.exit(1);
    }
    return expandHome(trimmed);
  }
  return path.join(os.homedir(), ".weave");
}

function expandHome(value) {
  if (value === "~") {
    return os.homedir();
  }
  if (value.startsWith("~/")) {
    return path.join(os.homedir(), value.slice(2));
  }
  return value;
}

function resolveWeaveBinaryPath() {
  const override = process.env.WEAVE_BINARY;
  if (override) {
    return override;
  }

  const targetTriple = resolveTargetTriple();
  const vendorBinary = path.join(
    __dirname,
    "..",
    "vendor",
    targetTriple,
    "weave",
    "weave",
  );
  if (fs.existsSync(vendorBinary)) {
    return vendorBinary;
  }

  const legacyVendorBinary = path.join(
    __dirname,
    "..",
    "vendor",
    "weave",
    targetTriple,
    "weave",
  );
  if (fs.existsSync(legacyVendorBinary)) {
    return legacyVendorBinary;
  }

  const repoBinary = path.join(__dirname, "..", "..", "weave");
  if (fs.existsSync(repoBinary)) {
    return repoBinary;
  }

  return null;
}

function resolveTargetTriple() {
  switch (process.arch) {
    case "x64":
      return "x86_64-apple-darwin";
    case "arm64":
      return "aarch64-apple-darwin";
    default:
      return "";
  }
}

function readPid() {
  if (!fs.existsSync(pidPath)) {
    return null;
  }
  const contents = fs.readFileSync(pidPath, "utf-8").trim();
  if (!contents) {
    return null;
  }
  const pid = Number.parseInt(contents, 10);
  return Number.isNaN(pid) ? null : pid;
}

function isRunning(pid) {
  try {
    process.kill(pid, 0);
    return true;
  } catch {
    return false;
  }
}

function writePid(pid) {
  fs.writeFileSync(pidPath, String(pid));
}

function clearPid() {
  if (fs.existsSync(pidPath)) {
    fs.unlinkSync(pidPath);
  }
}

function startService() {
  const existingPid = readPid();
  if (existingPid && isRunning(existingPid)) {
    // eslint-disable-next-line no-console
    console.log(`weave-service already running (pid ${existingPid}).`);
    return;
  }

  fs.mkdirSync(weaveHome, { recursive: true });
  if (existingPid) {
    clearPid();
  }

  const logFd = fs.openSync(logPath, "a");
  const child = spawn(binaryPath, ["-weave-home", weaveHome], {
    detached: true,
    stdio: ["ignore", logFd, logFd],
    env: { ...process.env, WEAVE_HOME: weaveHome },
  });
  child.unref();
  fs.closeSync(logFd);

  if (!child.pid) {
    // eslint-disable-next-line no-console
    console.error("Failed to start weave-service.");
    process.exit(1);
  }

  writePid(child.pid);
  // eslint-disable-next-line no-console
  console.log(`weave-service started (pid ${child.pid}).`);
}

async function stopService() {
  const pid = readPid();
  if (!pid) {
    // eslint-disable-next-line no-console
    console.log("weave-service is not running.");
    return;
  }

  if (!isRunning(pid)) {
    clearPid();
    // eslint-disable-next-line no-console
    console.log("weave-service is not running (stale pid file cleared).");
    return;
  }

  process.kill(pid, "SIGTERM");
  await waitForExit(pid, 5000);

  if (isRunning(pid)) {
    process.kill(pid, "SIGKILL");
    await waitForExit(pid, 1000);
  }

  clearPid();
  // eslint-disable-next-line no-console
  console.log(`weave-service stopped (pid ${pid}).`);
}

async function waitForExit(pid, timeoutMs) {
  const start = Date.now();
  while (Date.now() - start < timeoutMs) {
    if (!isRunning(pid)) {
      return;
    }
    await sleep(200);
  }
}

function sleep(ms) {
  return new Promise((resolve) => setTimeout(resolve, ms));
}
