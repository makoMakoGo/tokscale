#!/usr/bin/env node
import { spawnSync, execSync } from "node:child_process";
import { existsSync, readdirSync, realpathSync } from "node:fs";
import { resolve, join, basename } from "node:path";
import { fileURLToPath } from "node:url";

const binaryName = process.platform === "win32" ? "tokenx.exe" : "tokenx";

const currentDir = fileURLToPath(new URL(".", import.meta.url));
const dirName = basename(currentDir);
// In npm install: currentDir = .../node_modules/@juya-ai/tokenx/dist/
//   launcherDir = .../node_modules/@juya-ai/tokenx/
//   scopeDir = .../node_modules/@juya-ai/
// In monorepo dev (dist): currentDir = .../packages/tokenx/dist/
//   launcherDir = .../packages/tokenx/
//   scopeDir = .../packages/
// In monorepo dev (src): currentDir = .../packages/tokenx/src/
//   launcherDir = .../packages/tokenx/
//   scopeDir = .../packages/
const isSubDir = dirName === "dist" || dirName === "src";
const launcherDir = isSubDir ? resolve(currentDir, "..") : currentDir;
const scopeDir = resolve(launcherDir, "..");
const workspaceRoot = resolve(scopeDir, "..");

type LibcKind = "gnu" | "musl";

function detectLibcKind(): LibcKind | null {
  const override = process.env.TOKENX_LIBC?.trim().toLowerCase();
  if (override === "musl") return "musl";
  if (override === "gnu" || override === "glibc") return "gnu";

  const report = process.report?.getReport?.() as
    | {
        header?: {
          glibcVersionRuntime?: string;
          release?: { sourceUrl?: string };
        };
        sharedObjects?: string[];
      }
    | undefined;

  if (report?.header?.glibcVersionRuntime) {
    return "gnu";
  }

  if (
    Array.isArray(report?.sharedObjects) &&
    report.sharedObjects.some((obj) => obj.toLowerCase().includes("musl"))
  ) {
    return "musl";
  }

  // Bun reports neither glibcVersionRuntime nor sharedObjects, but its
  // release.sourceUrl names the build flavor (e.g. bun-linux-x64-musl-baseline.zip).
  if (report?.header?.release?.sourceUrl?.toLowerCase().includes("musl")) {
    return "musl";
  }

  try {
    const output = execSync("ldd --version", {
      encoding: "utf-8",
      stdio: ["ignore", "pipe", "pipe"],
    }).toLowerCase();
    if (output.includes("musl")) return "musl";
    if (output.includes("glibc") || output.includes("gnu")) return "gnu";
  } catch (error) {
    // musl's ldd rejects --version: it prints "musl libc" to stderr and
    // exits non-zero, so the answer is in the error, not the output.
    const { stdout, stderr } = (error ?? {}) as { stdout?: unknown; stderr?: unknown };
    const combined = `${stdout ?? ""}\n${stderr ?? ""}`.toLowerCase();
    if (combined.includes("musl")) return "musl";
    if (combined.includes("glibc") || combined.includes("gnu")) return "gnu";
  }

  // ldd missing or inconclusive: look for dynamic loaders. Either loader
  // can coexist with the other's libc (Debian's musl package installs
  // ld-musl-*; Alpine's gcompat installs ld-linux-*), so when both are
  // present, let the distro break the tie.
  const hasGnuLoader = loaderPresent("ld-linux-");
  const hasMuslLoader = loaderPresent("ld-musl-");
  if (hasGnuLoader !== hasMuslLoader) return hasMuslLoader ? "musl" : "gnu";
  if (hasGnuLoader && hasMuslLoader) {
    return existsSync("/etc/alpine-release") ? "musl" : "gnu";
  }

  return null;
}

// Glibc ships ld-linux-*.so.* in /lib64 (or /lib on some arches); musl
// distros (Alpine, Void-musl, ...) ship /lib/ld-musl-<arch>.so.1.
function loaderPresent(prefix: string): boolean {
  for (const dir of ["/lib", "/lib64"]) {
    try {
      if (readdirSync(dir).some((entry) => entry.startsWith(prefix))) {
        return true;
      }
    } catch {
      // Directory unreadable or missing; try the next one.
    }
  }
  return false;
}

function resolveTargetPackageName(): string | null {
  const arch = process.arch;

  if (process.platform === "darwin") {
    if (arch === "arm64") return "darwin-arm64";
    return null;
  }

  if (process.platform === "linux") {
    const libc = detectLibcKind();
    if (arch === "x64" && libc === "gnu") return "linux-x64-gnu";
    return null;
  }

  if (process.platform === "win32") {
    if (arch === "x64") return "win32-x64-msvc";
    return null;
  }

  return null;
}

function resolveRustTargetTriple(): string | null {
  const arch = process.arch;

  if (process.platform === "darwin") {
    if (arch === "arm64") return "aarch64-apple-darwin";
    return null;
  }

  if (process.platform === "linux") {
    const libc = detectLibcKind();
    if (arch === "x64" && libc === "gnu") return "x86_64-unknown-linux-gnu";
    return null;
  }

  if (process.platform === "win32") {
    if (arch === "x64") return "x86_64-pc-windows-msvc";
    return null;
  }

  return null;
}

const targetPackage = resolveTargetPackageName();
const searchPaths: string[] = [];

if (targetPackage) {
  const scopedPlatformPackage = `tokenx-${targetPackage}`;
  searchPaths.push(
    // npm/bun install: sibling scoped package (node_modules/@juya-ai/tokenx-<platform>/bin/...)
    join(scopeDir, scopedPlatformPackage, "bin", binaryName),
    // Nested node_modules: non-hoisted / pnpm (node_modules/@juya-ai/tokenx/node_modules/@juya-ai/tokenx-<platform>/bin/...)
    join(launcherDir, "node_modules", "@juya-ai", scopedPlatformPackage, "bin", binaryName),
    // Hoisted edge case (node_modules/@juya-ai/node_modules/@juya-ai/tokenx-<platform>/bin/...)
    join(scopeDir, "node_modules", "@juya-ai", scopedPlatformPackage, "bin", binaryName),
    join(workspaceRoot, "node_modules", "@juya-ai", scopedPlatformPackage, "bin", binaryName),
    // Monorepo development
    join(workspaceRoot, "packages", scopedPlatformPackage, "bin", binaryName),
  );
}

const rustTargetTriple = resolveRustTargetTriple();
if (rustTargetTriple) {
  searchPaths.push(join(workspaceRoot, "target", rustTargetTriple, "release", binaryName));
}

searchPaths.push(
  join(workspaceRoot, "target", "release", binaryName),
  join(launcherDir, "bin", binaryName),
);

function tryRealpath(p: string): string {
  try {
    return realpathSync(p);
  } catch {
    return p;
  }
}

// Paths that would re-enter this launcher if executed - using any of these as
// the "real" binary causes infinite recursion (a fork bomb). We compare by
// realpath so symlinks (e.g. npm/bun bin shims) are dereferenced.
const selfPaths = new Set<string>([
  tryRealpath(fileURLToPath(import.meta.url)),
  tryRealpath(join(launcherDir, "bin.js")),
]);
if (process.argv[1]) {
  selfPaths.add(tryRealpath(process.argv[1]));
}

function isSelfReference(p: string): boolean {
  return selfPaths.has(tryRealpath(p));
}

let binary = searchPaths.find((p) => existsSync(p) && !isSelfReference(p));

if (!binary) {
  console.error("Error: tokenx binary not found");
  console.error("Build from source: cargo build --release -p tokenx");
  if (targetPackage) {
    console.error(`Expected optional package: @juya-ai/tokenx-${targetPackage}`);
  }
  process.exit(1);
}

const result = spawnSync(binary, process.argv.slice(2), { stdio: "inherit" });
process.exit(result.status ?? 1);
