import fs from "fs";
import path from "path";
import { createHash } from "node:crypto";
import { ensureDataDir } from "../paths";
import { logger } from "../logger";

type BuildStream = (data: { type: string; text: string; level?: string }) => void;

/**
 * Fritter is distributed as a pinned release binary rather than vendored
 * source. v1.3 ships a Linux x64 generator and a Windows x64 executable; there
 * is no linux-arm64 asset, so Fritter is unavailable on ARM servers (Cupcake
 * remains the fallback there).
 */
const FRITTER_VERSION = "v1.3";
const RELEASE_BASE = `https://github.com/0xROOTPLS/Fritter/releases/download/${FRITTER_VERSION}`;

type ReleaseAsset = { filename: string; sha256: string };

function releaseAsset(): ReleaseAsset | null {
  if (process.platform === "win32") {
    return {
      filename: "fritter.exe",
      sha256: "38c8a7b1f71c176148ace08940c9bbe20856175526e9331d8526362e79982e2b",
    };
  }
  if (process.platform === "linux" && process.arch === "x64") {
    return {
      filename: "fritter-linux-x64",
      sha256: "dc83d66a9ae30903e5ad238b5db437bece366708285101b84173f0e826a81d65",
    };
  }
  return null;
}

function toolsDir(): string {
  return path.join(ensureDataDir(), "tools");
}

function binaryPath(): string {
  return path.join(toolsDir(), process.platform === "win32" ? "fritter.exe" : "fritter");
}

function cachePath(): string {
  return path.join(toolsDir(), "fritter.version");
}

function readCacheVersion(): string | null {
  try {
    const parsed = JSON.parse(fs.readFileSync(cachePath(), "utf8")) as { version?: unknown };
    return typeof parsed.version === "string" ? parsed.version : null;
  } catch {
    return null;
  }
}

function writeCacheVersion(): void {
  try {
    fs.mkdirSync(toolsDir(), { recursive: true });
    fs.writeFileSync(cachePath(), JSON.stringify({ version: FRITTER_VERSION }));
  } catch {}
}

function findSystemFritter(): string | null {
  for (const candidate of ["/usr/local/bin/fritter", "/usr/bin/fritter"]) {
    if (fs.existsSync(candidate)) return candidate;
  }
  try {
    const fromPath = Bun.which(process.platform === "win32" ? "fritter.exe" : "fritter");
    if (fromPath) return fromPath;
  } catch {}
  return null;
}

async function downloadAsset(
  asset: ReleaseAsset,
  dest: string,
  sendToStream?: BuildStream,
): Promise<boolean> {
  const url = `${RELEASE_BASE}/${asset.filename}`;
  const log = (text: string, level = "info") => {
    logger.info(`[fritter] ${text}`);
    sendToStream?.({ type: "output", text: `${text}\n`, level });
  };
  try {
    log(`Fritter: downloading ${FRITTER_VERSION} (${asset.filename})…`);
    const response = await fetch(url, { redirect: "follow" });
    if (!response.ok) {
      log(`Fritter: release download failed (${response.status})`, "error");
      return false;
    }
    const bytes = new Uint8Array(await response.arrayBuffer());
    const actual = createHash("sha256").update(bytes).digest("hex");
    if (actual !== asset.sha256) {
      log(`Fritter: checksum mismatch for ${asset.filename}`, "error");
      return false;
    }
    fs.mkdirSync(path.dirname(dest), { recursive: true });
    const temp = `${dest}.${process.pid}.tmp`;
    fs.writeFileSync(temp, bytes);
    fs.renameSync(temp, dest);
    try { fs.chmodSync(dest, 0o755); } catch {}
    log(`Fritter: ${FRITTER_VERSION} ready`);
    return true;
  } catch (error) {
    log(`Fritter: download error: ${error instanceof Error ? error.message : String(error)}`, "error");
    return false;
  }
}

/**
 * Returns a runnable Fritter generator, preferring the image-provided binary
 * (`/usr/local/bin/fritter`) and otherwise downloading the pinned release into
 * the persistent data directory.
 */
export async function ensureFritter(sendToStream?: BuildStream): Promise<string | null> {
  const installed = findSystemFritter();
  if (installed) return installed;

  const dest = binaryPath();
  if (fs.existsSync(dest) && readCacheVersion() === FRITTER_VERSION) return dest;

  const asset = releaseAsset();
  if (!asset) {
    const text = process.platform === "linux" && process.arch === "arm64"
      ? "Fritter v1.3 has no linux-arm64 release; use Cupcake on this server"
      : `Fritter v1.3 has no release for ${process.platform}/${process.arch}`;
    logger.warn(`[fritter] ${text}`);
    sendToStream?.({ type: "output", text: `${text}\n`, level: "warn" });
    return null;
  }

  if (!(await downloadAsset(asset, dest, sendToStream))) {
    if (fs.existsSync(dest)) return dest;
    return null;
  }
  writeCacheVersion();
  return dest;
}

/** Converts a Windows PE to raw x64 PIC shellcode with Fritter. */
export async function runFritter(
  inputPe: string,
  outputBin: string,
  sendToStream: BuildStream,
  opts?: {
    singleThreaded?: boolean;
    exitMode?: number;
    entropy?: number;
    preserveHeaders?: boolean;
    resumeOffset?: number;
  },
): Promise<boolean> {
  const bin = await ensureFritter(sendToStream);
  if (!bin) {
    sendToStream({ type: "output", text: "ERROR: Fritter binary not available\n", level: "error" });
    return false;
  }

  const args = [bin, "-i", inputPe, "-o", outputBin, "-f", "1"];
  if (opts?.singleThreaded) args.push("-t");
  if (opts?.exitMode !== undefined) args.push("-x", String(opts.exitMode));
  if (opts?.entropy !== undefined) args.push("-e", String(opts.entropy));
  if (opts?.preserveHeaders) args.push("-k", "2");
  if (opts?.resumeOffset !== undefined) args.push("-y", String(Math.trunc(opts.resumeOffset)));

  const result = Bun.spawnSync({
    cmd: args,
    cwd: process.cwd(),
    stdout: "pipe",
    stderr: "pipe",
    env: { ...process.env },
  });
  const stdout = result.stdout?.toString().trim() ?? "";
  if (stdout) sendToStream({ type: "output", text: stdout + "\n", level: "info" });
  if (result.exitCode !== 0) {
    const stderr = result.stderr?.toString().trim() ?? "";
    sendToStream({
      type: "output",
      text: `Fritter failed (exit ${result.exitCode})${stderr ? `: ${stderr}` : ""}\n`,
      level: "error",
    });
    return false;
  }
  const outputSize = fs.existsSync(outputBin) ? fs.statSync(outputBin).size : 0;
  if (outputSize === 0) {
    sendToStream({
      type: "output",
      text: "Fritter produced no output — check that the input PE is a valid Windows x64 image\n",
      level: "error",
    });
    return false;
  }
  return true;
}
