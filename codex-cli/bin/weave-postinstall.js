#!/usr/bin/env node
// Suggest restarting the Weave service after an npm update.

import crypto from "node:crypto";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";

if (process.platform !== "darwin") {
  process.exit(0);
}

const __filename = fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);

const weaveHome = resolveWeaveHome();
const pidPath = path.join(weaveHome, "weave-service.pid");
const metaPath = path.join(weaveHome, "weave-service.meta.json");

const pid = readPid();
if (!pid || !isRunning(pid)) {
  process.exit(0);
}

const binaryPath = resolveWeaveBinaryPath();
if (!binaryPath) {
  process.exit(0);
}

const currentHash = hashFile(binaryPath);
const metadata = readMetadata(metaPath);
const needsRestart =
  !metadata ||
  !metadata.binaryPath ||
  !metadata.binarySha256 ||
  !currentHash ||
  metadata.binaryPath !== binaryPath ||
  metadata.binarySha256 !== currentHash;

if (needsRestart) {
  // eslint-disable-next-line no-console
  console.log(
    "Weave updated; your service is still running the old version. Restart now: weave-service restart",
  );
}

function resolveWeaveHome() {
  const envValue = process.env.WEAVE_HOME;
  if (envValue !== undefined) {
    const trimmed = envValue.trim();
    if (!trimmed) {
      return path.join(os.homedir(), ".weave");
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

function readMetadata(filePath) {
  try {
    const contents = fs.readFileSync(filePath, "utf-8");
    return JSON.parse(contents);
  } catch {
    return null;
  }
}

function hashFile(filePath) {
  try {
    const data = fs.readFileSync(filePath);
    return crypto.createHash("sha256").update(data).digest("hex");
  } catch {
    return null;
  }
}
