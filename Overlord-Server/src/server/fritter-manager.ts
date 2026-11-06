import fs from "fs";
import path from "path";
import { ensureDataDir } from "../paths";
import { logger } from "../logger";

type BuildStream = (data: { type: string; text: string; level?: string }) => void;
import { resolveRuntimeRoot } from "./runtime-paths";

type CachedBuild = { cachedAt: number; sourceMtimeMs?: number; tag?: string };

// The vendored tree is written by the generator build (random headers and
// loader stubs). Serialize that work so parallel target builds cannot read a
// half-written tree.
let buildChain: Promise<void> = Promise.resolve();

function serializeBuild<T>(task: () => Promise<T>): Promise<T> {
  const run = buildChain.then(task, task);
  buildChain = run.then(() => undefined, () => undefined);
  return run;
}

type CompilerSet = {
  cc: string;
  hostCc: string;
  ar: string;
  objcopy: string;
};

/**
 * Fritter is a PIC shellcode generator (BSD-3-Clause) vendored under
 * `fritter/`. Unlike Cupcake it is x64-only, but every invocation and every
 * build of the generator re-randomizes the loader constants. On Windows the
 * generator is compiled with MinGW: the vendored aPLib `.lib` is MSVC COFF
 * with `coff64_` symbol prefixes, so the build extracts the objects and
 * renames the three aPLib entry points before linking.
 */

function toolsDir(): string {
  return path.join(ensureDataDir(), "tools");
}

function binaryPath(): string {
  return path.join(toolsDir(), process.platform === "win32" ? "fritter.exe" : "fritter");
}

function versionFilePath(): string {
  return path.join(toolsDir(), "fritter.version");
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

function findSystemFritter(): string | null {
  for (const c of ["/usr/local/bin/fritter", "/usr/bin/fritter"]) {
    if (fs.existsSync(c)) return c;
  }
  try {
    const p = Bun.which(process.platform === "win32" ? "fritter.exe" : "fritter");
    if (p) return p;
  } catch {}
  return null;
}

export function resolveFritterSourceDir(): string | null {
  const explicit = process.env.FRITTER_SOURCE_DIR?.trim();
  const candidates = [
    explicit ? path.resolve(explicit) : null,
    path.resolve(resolveRuntimeRoot(), "fritter"),
    path.resolve(process.cwd(), "fritter"),
    path.resolve(import.meta.dir, "../../../fritter"),
  ];
  for (const candidate of candidates) {
    if (candidate && fs.existsSync(path.join(candidate, "fritter.c"))) return candidate;
  }
  return null;
}

/** Newest mtime of the files that determine the generator binary. */
function sourceFingerprint(dir: string): number | null {
  const inputs = [
    "fritter.c",
    "hash.c",
    "encrypt.c",
    "format.c",
    "loader/clib.c",
    "loader/dispatch_shim.c",
    "loader/loader.c",
    "include/fritter.h",
    "include/aplib.h",
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

function winget(names: string[]): string | null {
  for (const n of names) {
    const p = Bun.which(n);
    if (p) return p;
  }
  return null;
}

function findCompilers(): CompilerSet | null {
  if (process.platform === "win32") {
    const cc = winget(["x86_64-w64-mingw32-gcc"]);
    const hostCc = winget(["gcc"]) ?? cc;
    if (!cc || !hostCc) return null;
    return {
      cc,
      hostCc,
      ar: winget(["ar", "x86_64-w64-mingw32-ar"]) ?? "ar",
      objcopy: winget(["objcopy", "x86_64-w64-mingw32-objcopy"]) ?? "objcopy",
    };
  }
  if (process.platform === "linux") {
    const cc = winget(["musl-gcc", "gcc"]);
    const hostCc = winget(["gcc"]);
    if (!cc || !hostCc) return null;
    // Linux uses the Makefile; the compiler set is only used by the Windows
    // recipe, so mirror the host tools for the shared helper.
    return { cc, hostCc, ar: "ar", objcopy: "objcopy" };
  }
  return null;
}

type RunResult = { code: number; out: string; err: string };

function run(cmd: string[], cwd: string): RunResult {
  const result = Bun.spawnSync({
    cmd,
    cwd,
    stdout: "pipe",
    stderr: "pipe",
    env: { ...process.env },
  });
  return {
    code: result.exitCode ?? -1,
    out: result.stdout?.toString() ?? "",
    err: result.stderr?.toString() ?? "",
  };
}

function runStep(
  log: (text: string) => void,
  cmd: string[],
  cwd: string,
  label: string,
): boolean {
  const r = run(cmd, cwd);
  if (r.code !== 0) {
    const detail = [r.err.trim(), r.out.trim()].filter(Boolean).join("\n");
    log(`Fritter: ${label} failed (exit ${r.code})${detail ? `: ${detail.slice(0, 800)}` : ""}`);
    return false;
  }
  return true;
}

async function buildWindows(
  sourceDir: string,
  out: string,
  sendToStream?: BuildStream,
): Promise<string | null> {
  const log = (text: string) => {
    logger.info(`[fritter] ${text.trim()}`);
    sendToStream?.({ type: "output", text: `${text}\n`, level: "info" });
  };
  const compilers = findCompilers();
  if (!compilers) {
    log("Fritter: x86_64-w64-mingw32-gcc and gcc are required on Windows");
    return null;
  }

  const buildDir = path.join(sourceDir, ".fritter-build");
  const aplibDir = path.join(buildDir, "aplib");
  fs.rmSync(buildDir, { recursive: true, force: true });
  fs.mkdirSync(aplibDir, { recursive: true });

  const genPoly = path.join(buildDir, "gen_poly.exe");
  const genApi = path.join(buildDir, "gen_api_shuffle.exe");
  const exe2h = path.join(buildDir, "exe2h.exe");
  const genOut = path.join(buildDir, "fritter.exe");

  log("Fritter: compiling generator from vendored source (MinGW)…");
  if (!runStep(log, [compilers.hostCc, "tools/gen_poly.c", "-o", genPoly], sourceDir, "gen_poly compile")) return null;
  if (!runStep(log, [genPoly, "include/poly_seed.h"], sourceDir, "gen_poly run")) return null;
  if (!runStep(log, [compilers.hostCc, "tools/gen_api_shuffle.c", "-o", genApi], sourceDir, "gen_api_shuffle compile")) return null;
  if (!runStep(log, [genApi, "include/api_master.h", "include/api_shuffle.h"], sourceDir, "gen_api_shuffle run")) return null;
  if (!runStep(log, [compilers.hostCc, "-I", "include", "loader/exe2h/exe2h.c", "loader/exe2h/mmap-windows.c", "-lshlwapi", "-o", exe2h], sourceDir, "exe2h compile")) return null;

  const loaderFlags = ["-fno-toplevel-reorder", "-fno-builtin", "-fpack-struct=8", "-fPIC", "-O1", "-nostdlib"];
  const loaderSources = ["loader/loader.c", "loader/depack.c", "loader/clib.c", "hash.c", "encrypt.c"];
  for (const order of [1, 2]) {
    const loaderExe = path.join(buildDir, `loader_peb${order}.exe`);
    if (!runStep(log, [
      compilers.cc, `-DPEB_WALK_ORDER=${order}`, ...loaderFlags, ...loaderSources,
      "-I", "include", "-o", loaderExe,
    ], sourceDir, `loader PEB${order} compile`)) return null;
    if (!runStep(log, [exe2h, loaderExe], sourceDir, `loader PEB${order} header`)) return null;
  }
  const shimExe = path.join(buildDir, "dispatch_shim.exe");
  if (!runStep(log, [
    compilers.cc, ...loaderFlags, "loader/dispatch_shim.c", "-I", "include", "-o", shimExe,
  ], sourceDir, "dispatch shim compile")) return null;
  if (!runStep(log, [exe2h, shimExe], sourceDir, "dispatch shim header")) return null;

  // aPLib is distributed as an MSVC COFF library with coff64_ prefixes.
  if (!runStep(log, [compilers.ar, "x", path.join(sourceDir, "lib", "aplib64.lib")], aplibDir, "aplib extract")) return null;
  const objFiles = fs.readdirSync(aplibDir).filter((f) => f.toLowerCase().endsWith(".obj"));
  if (objFiles.length === 0) {
    log("Fritter: aplib64.lib contained no COFF objects");
    return null;
  }
  for (const obj of objFiles) {
    const p = path.join(aplibDir, obj);
    if (!runStep(log, [
      compilers.objcopy,
      "--redefine-sym", "coff64_aP_pack=aP_pack",
      "--redefine-sym", "coff64_aP_workmem_size=aP_workmem_size",
      "--redefine-sym", "coff64_aP_max_packed_size=aP_max_packed_size",
      p, p,
    ], sourceDir, `aplib symbol rewrite (${obj})`)) return null;
  }

  if (!runStep(log, [
    compilers.cc, "-Wall", "-Os", "-s", "-fpack-struct=8", "-DFRITTER_EXE",
    "-Wno-format-truncation", "-I", "include", "fritter.c", "hash.c", "encrypt.c",
    "format.c", "loader/clib.c", ...objFiles.map((f) => path.join(aplibDir, f)),
    "-lshlwapi", "-o", genOut,
  ], sourceDir, "fritter link")) return null;

  if (!fs.existsSync(genOut)) return null;
  fs.mkdirSync(path.dirname(out), { recursive: true });
  fs.copyFileSync(genOut, out);
  try { fs.chmodSync(out, 0o755); } catch {}
  try { fs.rmSync(buildDir, { recursive: true, force: true }); } catch {}
  // The generator's random seed lives only in these generated headers; keep
  // the vendored tree free of per-build artifacts so source control stays clean.
  for (const rel of [
    "loader_peb1_exe_x64.go", "loader_peb1_exe_x64.h",
    "loader_peb2_exe_x64.go", "loader_peb2_exe_x64.h",
    "dispatch_shim_exe_x64.go", "dispatch_shim_exe_x64.h",
    "loader_peb1_fn_table_x64.h", "loader_peb1_ref_table_x64.h",
    "loader_peb2_fn_table_x64.h", "loader_peb2_ref_table_x64.h",
    "dispatch_shim_fn_table_x64.h", "dispatch_shim_ref_table_x64.h",
    "include/poly_seed.h", "include/api_shuffle.h",
  ]) {
    try { fs.rmSync(path.join(sourceDir, rel), { force: true }); } catch {}
  }
  return out;
}

async function buildLinux(
  sourceDir: string,
  out: string,
  sendToStream?: BuildStream,
): Promise<string | null> {
  const log = (text: string) => {
    logger.info(`[fritter] ${text.trim()}`);
    sendToStream?.({ type: "output", text: `${text}\n`, level: "info" });
  };
  const make = winget(["make"]);
  if (!make) {
    log("Fritter: make is required to build the Linux generator");
    return null;
  }
  log("Fritter: running make -f Makefile.linux…");
  if (!runStep(log, [make, "-f", "Makefile.linux"], sourceDir, "make")) return null;
  const built = path.join(sourceDir, "fritter");
  if (!fs.existsSync(built)) {
    log("Fritter: make completed but fritter was not produced");
    return null;
  }
  fs.mkdirSync(path.dirname(out), { recursive: true });
  fs.copyFileSync(built, out);
  try { fs.chmodSync(out, 0o755); } catch {}
  // Leave the vendored tree source-only; the next build regenerates these.
  run([make, "-f", "Makefile.linux", "clean"], sourceDir);
  return out;
}

async function buildFromSource(
  sendToStream?: BuildStream,
): Promise<string | null> {
  const sourceDir = resolveFritterSourceDir();
  if (!sourceDir) {
    const text = "Fritter: vendored source not found (fritter/)";
    logger.warn(`[fritter] ${text}`);
    sendToStream?.({ type: "output", text: `${text}\n`, level: "warn" });
    return null;
  }
  const out = binaryPath();
  if (process.platform === "win32") return buildWindows(sourceDir, out, sendToStream);
  if (process.platform === "linux") return buildLinux(sourceDir, out, sendToStream);
  const text = `Fritter: source build is not supported on ${process.platform}`;
  logger.warn(`[fritter] ${text}`);
  sendToStream?.({ type: "output", text: `${text}\n`, level: "warn" });
  return null;
}

/**
 * Ensures the Fritter generator is available.
 *
 * `force` rebuilds from source so the loader constants (cipher rotation, API
 * table order, PEB walk direction, shim scrambling) are fresh for this
 * payload. When a rebuild fails but a previous binary exists, the stale binary
 * is used with a warning rather than failing the whole build.
 */
export async function ensureFritter(
  sendToStream?: BuildStream,
  force = false,
): Promise<string | null> {
  const bin = binaryPath();
  const sourceDir = resolveFritterSourceDir();
  const sourceMtimeMs = sourceDir ? sourceFingerprint(sourceDir) : null;
  const cached = readCache();

  const log = (text: string, level = "info") => {
    logger.info(`[fritter] ${text.trim()}`);
    sendToStream?.({ type: "output", text: `${text}\n`, level });
  };

  if (!force && fs.existsSync(bin) && sourceMtimeMs !== null && cached?.sourceMtimeMs === sourceMtimeMs) {
    return bin;
  }

  if (force || !fs.existsSync(bin)) {
    const built = await serializeBuild(() => buildFromSource(sendToStream));
    if (built) {
      writeCache({ cachedAt: Date.now(), sourceMtimeMs: sourceMtimeMs ?? undefined, tag: `build @${sourceMtimeMs ?? 0}` });
      log("Fritter: ready (fresh build)");
      return built;
    }
    if (fs.existsSync(bin)) {
      log("Fritter: rebuild failed — using existing generator binary", "warn");
      return bin;
    }
    const sysBin = findSystemFritter();
    if (sysBin) {
      log(`Fritter: using system binary at ${sysBin}`, "warn");
      return sysBin;
    }
    log("Fritter Shellcode mode requires fritter/ source plus MinGW (Windows) or make + mingw-w64/musl (Linux)", "error");
    return null;
  }

  if (fs.existsSync(bin)) {
    log(`Fritter: using compiled binary (${cached?.tag ?? "built"})`);
    return bin;
  }

  const sysBin = findSystemFritter();
  if (sysBin) return sysBin;
  return buildFromSource(sendToStream);
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
  const bin = await ensureFritter(sendToStream, true);
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

  const r = run(args, process.cwd());
  const stdout = r.out.trim();
  if (stdout) sendToStream({ type: "output", text: stdout + "\n", level: "info" });
  if (r.code !== 0) {
    const stderr = r.err.trim();
    sendToStream({ type: "output", text: `Fritter failed (exit ${r.code})${stderr ? `: ${stderr}` : ""}\n`, level: "error" });
    return false;
  }
  const outSize = fs.existsSync(outputBin) ? fs.statSync(outputBin).size : 0;
  if (outSize === 0) {
    sendToStream({ type: "output", text: "Fritter produced no output — check that the input PE is a valid Windows x64 image\n", level: "error" });
    return false;
  }
  return true;
}
