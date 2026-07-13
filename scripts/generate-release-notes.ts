#!/usr/bin/env bun
/**
 * Usage: bun scripts/generate-release-notes.ts <version>
 * Env: GITHUB_REPOSITORY (default: makoMakoGo/tokscale)
 */
export {};

import { execFileSync } from "node:child_process";

const REPO = process.env.GITHUB_REPOSITORY || "makoMakoGo/tokscale";

interface Commit {
  hash: string;
  message: string;
}

interface PRInfo {
  number: number;
  title: string;
  url: string;
}

interface ChangeEntry {
  message: string;
  url: string;
  prNumber?: number;
}

function run(command: string, args: string[]): string {
  try {
    return execFileSync(command, args, {
      encoding: "utf8",
      stdio: ["ignore", "pipe", "pipe"],
    }).trim();
  } catch (error) {
    if (error instanceof Error) {
      throw new Error(`${command} ${args.join(" ")} failed: ${error.message}`);
    }
    throw error;
  }
}

function runJson<T>(command: string, args: string[]): T {
  const output = run(command, args);
  if (!output) {
    throw new Error(`${command} ${args.join(" ")} returned no JSON`);
  }
  try {
    return JSON.parse(output) as T;
  } catch (error) {
    throw new Error(`${command} ${args.join(" ")} returned invalid JSON`, {
      cause: error,
    });
  }
}

function getPreviousTag(): string | null {
  const headLine = run("git", ["rev-list", "--parents", "-n", "1", "HEAD"]);
  const [, firstParent] = headLine.split(" ");
  if (!firstParent) return null;

  const candidate = run(
    "git",
    ["describe", "--first-parent", "--tags", "--abbrev=0", "--always", firstParent]
  );
  const matchingTag = run("git", ["tag", "--list", candidate]);
  return matchingTag === candidate ? candidate : null;
}

function getCommitsBetween(fromTag: string, toRef: string): Commit[] {
  const output = run("git", [
    "log",
    `${fromTag}..${toRef}`,
    "--first-parent",
    "--no-merges",
    "--reverse",
    "--format=%H%x1f%s",
  ]);
  if (!output) return [];
  return output
    .split("\n")
    .filter((line) => line.trim())
    .map((line) => {
      const [hash = "", message = ""] = line.split("\x1f");
      return { hash, message };
    })
    .filter(
      (entry) =>
        entry.hash && !/^chore(?:\(release\))?: bump version\b/.test(entry.message)
    );
}

function findRepositoryPullRequest(commitHash: string): PRInfo | null {
  const result = runJson<
    Array<{
      number?: number;
      title?: string;
      merged_at?: string | null;
      html_url?: string;
      base?: { repo?: { full_name?: string } };
    }>
  >("gh", ["api", `repos/${REPO}/commits/${commitHash}/pulls`]);
  const repositoryName = REPO.toLowerCase();
  const pr = result.find(
    (candidate) =>
      candidate.merged_at != null &&
      candidate.base?.repo?.full_name?.toLowerCase() === repositoryName
  );
  if (!pr) return null;
  if (!pr.number || !pr.title || !pr.html_url) {
    throw new Error(
      `GitHub returned incomplete pull request metadata for ${commitHash}`
    );
  }
  return { number: pr.number, title: pr.title, url: pr.html_url };
}

function markdownLinkText(value: string): string {
  return value
    .replaceAll("\\", "\\\\")
    .replaceAll("[", "\\[")
    .replaceAll("]", "\\]");
}

function generateReleaseNotes(version: string): string {
  const prevTag = getPreviousTag();
  if (!prevTag) {
    return [
      "First public npm release of the independently maintained, local-first Tokscale fork.",
      "",
      "## Distribution",
      "",
      "This release establishes the fork package namespace:",
      "",
      "- `@juya-ai/tokscale`",
      "- `@juya-ai/tokscale-cli`",
      "- `@juya-ai/tokscale-cli-darwin-arm64`",
      "- `@juya-ai/tokscale-cli-linux-x64-gnu`",
      "- `@juya-ai/tokscale-cli-win32-x64-msvc`",
      "",
      "The installed command remains `tokscale`. These packages are separate from the upstream `tokscale` npm distribution.",
      "",
      "## Install",
      "",
      "```bash",
      `npm install -g @juya-ai/tokscale@${version}`,
      "tokscale --version",
      "```",
    ].join("\n");
  }

  const commits = getCommitsBetween(prevTag, "HEAD");
  const entries: ChangeEntry[] = [];
  const seenPRs = new Set<number>();

  for (const commit of commits) {
    const prInfo = findRepositoryPullRequest(commit.hash);

    if (prInfo?.number && seenPRs.has(prInfo.number)) {
      continue;
    }

    if (prInfo?.number) {
      seenPRs.add(prInfo.number);
    }

    entries.push({
      message: prInfo?.title || commit.message,
      url: prInfo?.url || `https://github.com/${REPO}/commit/${commit.hash}`,
      prNumber: prInfo?.number,
    });
  }

  if (entries.length === 0) {
    throw new Error(`No fork changes found between ${prevTag} and HEAD`);
  }

  const lines: string[] = [
    `Fork release of \`@juya-ai/tokscale\` version \`${version}\`.`,
    "",
    `## Changes since ${prevTag}`,
    "",
  ];

  for (const entry of entries) {
    lines.push(`- [${markdownLinkText(entry.message)}](${entry.url})`);
  }

  lines.push(
    "",
    "## Install",
    "",
    "```bash",
    `npm install -g @juya-ai/tokscale@${version}`,
    "```"
  );

  return lines.join("\n");
}

function main(): void {
  const version = process.argv[2];
  if (!version) {
    console.error("Usage: bun scripts/generate-release-notes.ts <version>");
    process.exit(1);
  }
  const notes = generateReleaseNotes(version);
  console.log(notes);
}

main();
