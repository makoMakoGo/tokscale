# Supported clients and data locations

The canonical identity list is `crates/tokscale-core/client-catalog.json`. It is
used to generate Rust client identity data. This page summarizes scan inputs
and semantic boundaries for users.

Use the Models report to scan one Client and inspect its Data Health:

```bash
bun run cli -- models --client codex --json --no-spinner
```

The retired `clients` diagnostic command is not a second discovery authority.
The table below and each Client adapter own the documented locations; actual
Input failures are reported in Models JSON or the TUI Data Health view.

## Client table

| ID | Display name | Local inputs | Notes |
| --- | --- | --- | --- |
| `opencode` | OpenCode | `~/.local/share/opencode/opencode*.db` | Reads only current-format SQLite databases and combines multiple release channels when present. |
| `claude` | Claude | `~/.claude/projects/**/*.jsonl`, `~/.claude/transcripts/**/*.jsonl` | Claude Desktop chat history is not treated as Claude Code token accounting. |
| `codex` | Codex CLI | `$CODEX_HOME/sessions/**/*.jsonl`, fallback `~/.codex/sessions/` | Includes provider-owned interactive and `codex exec` sessions. |
| `gemini` | Gemini CLI | `$GEMINI_CLI_HOME/tmp/**/chats/*`, fallback `~/.gemini/tmp/` | Reads local chat files. |
| `amp` | Amp | `~/.local/share/amp/threads/T-*.json` | Reads local thread files. |
| `droid` | Droid | `~/.factory/sessions/**/*.settings.json`, related session JSONL and Mission `features.json` | Reads Factory Droid sessions and attributes subagent usage to `Droid Explorer`, `Droid Worker`, `Droid Orchestrator`, or `Droid Validator`. |
| `openclaw` | OpenClaw | `~/.openclaw/agents/` plus legacy `.clawdbot`, `.moltbot`, `.moldbot` roots | Reads agent session indexes and JSONL session files. |
| `pi` | Pi | `~/.pi/agent/sessions/**/*.jsonl` | Separate from OMP by design. |
| `omp` | OMP | `~/.omp/agent/sessions/**/*.jsonl` | Separate from Pi by design. |
| `kimi` | Kimi | `$KIMI_CODE_HOME/sessions/**/agents/*/wire.jsonl` (`KIMI_CODE_HOME` defaults to `~/.kimi-code`) | Reads current-layout per-agent request and usage records. |
| `qwen` | Qwen CLI | `~/.qwen/projects/**/*.jsonl` | Reads Qwen chat JSONL files. |
| `roocode` | Roo Code | `~/.config/Code/User/globalStorage/rooveterinaryinc.roo-cline/tasks/**/ui_messages.json` | Also scans VS Code server globalStorage where supported. |
| `mux` | Mux | `~/.mux/sessions/**/session-usage.json` | Reads per-session usage summaries. |
| `kilo` | Kilo | `~/.local/share/kilo/kilo.db` | Reads the current SQLite store shared by Kilo frontends running under the same OS user and data environment; see [verified storage facts](facts/kilo.md). |
| `hermes` | Hermes Agent | `$HERMES_HOME/state.db`, fallback `~/.hermes/state.db` | Ignores app cost fields and derives cost from tokens. |
| `copilot` | Copilot | `~/.copilot/otel/*.jsonl` or `COPILOT_OTEL_FILE_EXPORTER_PATH` | Requires Copilot OTEL file export. |
| `goose` | Goose | `~/.local/share/goose/sessions/sessions.db` and platform legacy roots | `GOOSE_PATH_ROOT` can point at an alternate root. |
| `codebuff` | Codebuff | `$CODEBUFF_DATA_DIR/projects/**/chat-messages.json`, fallback `~/.config/manicode/projects/` | Also scans dev/staging Manicode roots. |
| `codebuddy` | CodeBuddy | `~/.codebuddy/projects/**/*.jsonl` and local CodeBuddy/VS Code extension logs | Reads assistant/function-call usage and final agent usage from local CodeBuddy records. |
| `antigravity` | Antigravity | `$GEMINI_CLI_HOME/antigravity-cli/conversations/*.db`, fallback `~/.gemini/antigravity-cli/conversations/*.db` | Reads current AGY CLI SQLite/WAL data directly. Antigravity IDE and Antigravity 2.0 Agent Manager are intentionally unsupported; see ADR 0025. |
| `zed` | Zed Agent | `~/.local/share/zed/threads/threads.db` | Hosted Zed model usage only; external ACP agents are not included. |
| `zcode` | ZCode | `~/.zcode/projects/**/*.jsonl` | Reads Z.ai ADE JSONL sessions. |
| `kiro` | Kiro | `~/.kiro/sessions/cli/`, `~/.local/share/kiro-cli/data.sqlite3`, and Kiro IDE globalStorage snapshots | Combines CLI and IDE local inputs when present. |
| `junie` | Junie | `~/.junie/sessions/**/events.jsonl` | Reads JetBrains Junie session events. |
| `cline` | Cline | `$CLINE_SESSION_DATA_DIR/**/*.messages.json`, then `$CLINE_DATA_DIR/sessions`, `$CLINE_DIR/data/sessions`, or `~/.cline/data/sessions` | Reads the shared SDK v1 artifacts written by VS Code 4.0+ and CLI 3.x. Retired VS Code globalStorage task logs are unsupported. |
| `commandcode` | Command Code | `~/.commandcode/projects/**/*.jsonl` | Estimated from transcripts. |
| `grok` | Grok Build | `$GROK_HOME/sessions/**/updates.jsonl`, fallback `~/.grok/sessions/` | Reads total-token deltas and applies the fixed total-only bucket allocation from ADR 0017. |
| `warp` | Warp/Oz | `~/.local/state/warp-terminal/warp.sqlite` on Linux, Warp App Group/Application Support on macOS, `%LOCALAPPDATA%\warp\Warp\data\warp.sqlite` on Windows | Reads local per-conversation, per-model token totals and applies the fixed total-only bucket allocation from ADR 0017. |

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
      ],
      "warp": [
        "/mnt/c/Users/me/AppData/Local/warp/Warp/data"
      ]
    }
  }
}
```

OpenCode SQLite files are configured separately because they are database files,
not recursive scan roots:

```json
{
  "scanner": {
    "opencodeDbPaths": [
      "/Users/me/Library/Application Support/opencode/opencode-stable.db"
    ]
  }
}
```

`scanner.opencodeDbPaths` is the only persistent custom OpenCode input.
`scanner.extraScanPaths.opencode` and `TOKSCALE_EXTRA_DIRS` entries for
OpenCode are ignored. Its configured file paths are authoritative, so missing
or unreadable paths appear as `input-unavailable` in report health. Legacy
`storage/message/**/*.json` data is not read. `NotFound` during automatic
discovery is treated as absent; every other discovery I/O failure remains
visible in health. Databases without the current session schema, or with
malformed current message payloads, likewise produce incomplete/degraded input
rather than a clean empty input, without aborting unrelated clients. Current
payloads must include role, model, timestamp, token, and cache-token fields.
Provider is optional identity metadata: a missing or blank value is inferred
from the model when possible and otherwise becomes `unknown`. Blank model or
session identifiers and timestamps that are non-positive, non-finite, or not
exactly representable as `i64` are rejected. Explicit `tokens: null`,
non-assistant messages, and zero positive usage are filtered.

Use `TOKSCALE_EXTRA_DIRS` for one-off runs:

```bash
TOKSCALE_EXTRA_DIRS='codex:/abs/path/.codex/sessions,gemini:/abs/path/gemini/tmp' \
  tokscale models --no-spinner
```

## Integration data boundaries

### Kimi model identity

Kimi Code stores an alias on each `usage.record`. When the same agent wire has a
valid preceding `llm.request` for that alias, Tokscale uses the physical model
from its latest such request; otherwise an exact current-config entry may enrich
it. If neither exists, Tokscale retains the alias and all valid token buckets,
then infers a provider or records `unknown`. Request transport is not treated as
model ownership. The
older root-level Kimi CLI session format is not supported. See
[the verified Kimi storage facts](facts/kimi-code.md) and
[ADR 0020](adr/0020-input-ingestion-and-integrity-contract.md).

Antigravity is not a cache-backed integration. Reports and the TUI read current
AGY CLI databases directly; there is no sync command. Historical
`~/.config/tokscale/antigravity-cache/` artifacts are ignored and may be
deleted.

`warp` has one maintained surface: local reports read `warp.sqlite` when it is
available. Those rows are per-conversation/per-model aggregates, not turns;
they are timestamped with the conversation `last_modified_at` value when
present, then the latest query timestamp, then file mtime. Warp's naive SQLite
timestamps are interpreted as UTC. Tokscale does not own Warp credentials,
remote quota caches, login, logout, or sync commands.

## Retention notes

Some upstream tools delete old sessions automatically. If complete local
history matters, configure retention in the originating client before data expires.

Claude Code defaults to a finite cleanup period in some configurations. Gemini
CLI, Codex CLI, and OpenCode generally keep local sessions unless the user or
tool configuration removes them.
