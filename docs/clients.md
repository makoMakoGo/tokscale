# Supported clients and data locations

The canonical identity list is `crates/tokscale-core/client-catalog.json`. It is
used to generate Rust client identity data. This page summarizes scan sources
and semantic boundaries for users.

Run this command from a built checkout to inspect what Tokscale sees on the
current machine:

```bash
bun run cli -- clients
bun run cli -- clients --json
```

When using an installed binary, use `tokscale clients` instead.

## Client table

| ID | Display name | Local source | Notes |
| --- | --- | --- | --- |
| `opencode` | OpenCode | `~/.local/share/opencode/opencode*.db` and legacy `~/.local/share/opencode/storage/message/` | Scans multiple release-channel databases when present. |
| `claude` | Claude Code | `~/.claude/projects/**/*.jsonl`, `~/.claude/transcripts/**/*.jsonl` | Claude Desktop chat history is not treated as Claude Code token accounting. |
| `codex` | Codex CLI | `$CODEX_HOME/sessions/**/*.jsonl`, fallback `~/.codex/sessions/` | Also supports `tokscale headless codex ...` capture. |
| `cursor` | Cursor | `~/.config/tokscale/cursor-cache/usage*.csv` | Reads a local API cache. Logged-in reports and the TUI may auto-refresh stale cache data; local `~/.cursor` state is not parsed. |
| `gemini` | Gemini CLI | `$GEMINI_CLI_HOME/tmp/**/chats/*`, fallback `~/.gemini/tmp/` | Reads local chat files. |
| `amp` | Amp | `~/.local/share/amp/threads/T-*.json` | Reads local thread files. |
| `droid` | Droid | `~/.factory/sessions/**/*.settings.json` | Reads Factory Droid sessions. |
| `openclaw` | OpenClaw | `~/.openclaw/agents/` plus legacy `.clawdbot`, `.moltbot`, `.moldbot` roots | Reads agent session indexes and JSONL session files. |
| `pi` | Pi | `~/.pi/agent/sessions/**/*.jsonl` | Separate from OMP by design. |
| `omp` | OMP | `~/.omp/agent/sessions/**/*.jsonl` | Separate from Pi by design. |
| `kimi` | Kimi | `$KIMI_CODE_HOME/sessions/**/wire.jsonl`, fallback `~/.kimi-code/sessions/` | Reads `usage.record` rows. |
| `qwen` | Qwen CLI | `~/.qwen/projects/**/*.jsonl` | Reads Qwen chat JSONL files. |
| `roocode` | Roo Code | `~/.config/Code/User/globalStorage/rooveterinaryinc.roo-cline/tasks/**/ui_messages.json` | Also scans VS Code server globalStorage where supported. |
| `kilocode` | KiloCode | `~/.config/Code/User/globalStorage/kilocode.kilo-code/tasks/**/ui_messages.json` | Same task-log family as Roo Code. |
| `mux` | Mux | `~/.mux/sessions/**/session-usage.json` | Reads per-session usage summaries. |
| `kilo` | Kilo CLI | `~/.local/share/kilo/kilo.db` | Reads local SQLite data. |
| `hermes` | Hermes Agent | `$HERMES_HOME/state.db`, fallback `~/.hermes/state.db` | Ignores app cost fields and derives cost from tokens. |
| `copilot` | Copilot | `~/.copilot/otel/*.jsonl` or `COPILOT_OTEL_FILE_EXPORTER_PATH` | Requires Copilot OTEL file export. |
| `goose` | Goose | `~/.local/share/goose/sessions/sessions.db` and platform legacy roots | `GOOSE_PATH_ROOT` can point at an alternate root. |
| `codebuff` | Codebuff | `$CODEBUFF_DATA_DIR/projects/**/chat-messages.json`, fallback `~/.config/manicode/projects/` | Also scans dev/staging Manicode roots. |
| `antigravity` | Antigravity | `~/.config/tokscale/antigravity-cache/sessions/*.jsonl` and Antigravity CLI conversation databases | IDE data requires `tokscale antigravity sync`; CLI databases are read directly. |
| `zed` | Zed Agent | `~/.local/share/zed/threads/threads.db` | Hosted Zed model usage only; external ACP agents are not included. |
| `zcode` | ZCode | `~/.zcode/projects/**/*.jsonl` | Reads Z.ai ADE JSONL sessions. |
| `kiro` | Kiro | `~/.kiro/sessions/cli/`, `~/.local/share/kiro-cli/data.sqlite3`, and Kiro IDE globalStorage snapshots | Combines CLI and IDE local sources when present. |
| `junie` | Junie | `~/.junie/sessions/**/events.jsonl` | Reads JetBrains Junie session events. |
| `trae` | Trae | `~/.config/tokscale/trae-cache/sessions/*.json` | Requires `tokscale trae login` and `tokscale trae sync`. China variants are not supported. |
| `cline` | Cline | VS Code globalStorage `saoudrizwan.claude-dev/tasks/**/ui_messages.json` | Same task-log family as Roo Code and KiloCode. |
| `commandcode` | Command Code | `~/.commandcode/projects/**/*.jsonl` | Estimated from transcripts. |
| `grok` | Grok Build | `$GROK_HOME/sessions/**/updates.jsonl`, fallback `~/.grok/sessions/` | Reads Grok session update logs. |
| `crush` | Crush | `~/.local/share/crush/projects.json` identity only | Disabled for normal local token reports; no accepted token-level source. |
| `warp` | Warp/Oz | `~/.config/tokscale/warp-cache/usage*.json` | Subscription aggregate surface only; not normal local token reports. |

## Extra scan roots

Use `scanner.extraScanPaths` in `settings.json` for persistent extra roots:

```json
{
  "scanner": {
    "extraScanPaths": {
      "codex": [
        "/Users/me/workspace/project-a/.codex/sessions"
      ],
      "gemini": [
        "/Users/me/imports/old-machine/gemini/tmp"
      ],
      "hermes": [
        "/Users/me/.hermes/profiles/research/state.db"
      ],
      "zed": [
        "/mnt/c/Users/me/AppData/Local/Zed/threads"
      ]
    }
  }
}
```

Use `TOKSCALE_EXTRA_DIRS` for one-off runs:

```bash
TOKSCALE_EXTRA_DIRS='codex:/abs/path/.codex/sessions,gemini:/abs/path/gemini/tmp' \
  tokscale --no-spinner --light
```

## Cache-backed integrations

Cursor reads a local API cache, but it is not purely manual-sync-backed. Ordinary
local reports and the TUI may call the Cursor API before reading reports when
all of these are true:

- no `--home` override is active;
- the client filter includes Cursor, including the default unfiltered report;
- saved Cursor credentials exist;
- the expected Cursor cache files are older than five minutes.

Manual commands are still available:

```bash
tokscale cursor login --name work
tokscale cursor sync --json
```

`tokscale cursor sync --json` forces a refresh. Filtering Cursor out with
`--client` or using `--home` prevents the implicit pre-report refresh.

Antigravity and Trae are different: they do not refresh from the root report or
TUI command. Run their sync commands before reports when you need fresh data:

```bash
tokscale antigravity status
tokscale antigravity sync

tokscale trae login
tokscale trae sync --since 30
```

`warp` is also sync-backed, but its data belongs to subscription usage rather
than normal local token reports:

```bash
tokscale warp login
tokscale warp sync --json
tokscale usage
```

## Retention notes

Some upstream tools delete old sessions automatically. If complete local
history matters, configure retention in the source client before data expires.

Claude Code defaults to a finite cleanup period in some configurations. Gemini
CLI, Codex CLI, and OpenCode generally keep local sessions unless the user or
tool configuration removes them.
