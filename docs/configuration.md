# Configuration

Tokscale stores most local settings under the platform config directory:

- Linux/macOS default: `~/.config/tokscale/settings.json`
- Windows default: `%APPDATA%\tokscale\settings.json`
- Override root: `TOKSCALE_CONFIG_DIR`

## Example

```json
{
  "colorPalette": "blue",
  "includeUnusedModels": false,
  "defaultClients": ["opencode", "claude"],
  "usageTabEnabled": true,
  "usageProviders": ["codex", "zai", "minimax-token-plan-cn"],
  "scanner": {
    "opencodeDbPaths": [
      "/Users/me/Library/Application Support/opencode/opencode-stable.db"
    ],
    "extraScanPaths": {
      "codex": [
        "/Users/me/workspace/project-a/.codex/sessions"
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

## Settings

| Setting | Type | Meaning |
| --- | --- | --- |
| `colorPalette` | string | TUI color theme. Known values include `green`, `halloween`, `teal`, `blue`, `pink`, `purple`, `orange`, `monochrome`, `ylgnbu`, `graphite`, `lagoon`, and `dusk`. An explicit `--theme` overrides this saved value. |
| `includeUnusedModels` | boolean | Show zero-token models in reports. |
| `autoRefreshEnabled` | boolean | Enable background TUI refresh of the fixed startup client universe. |
| `autoRefreshMs` | number | Background TUI refresh interval in milliseconds. View changes do not reset it. |
| `defaultClients` | string[] | Default scan scope when no `--client/-c` flag is passed. Reports use it for that invocation; the TUI fixes it as the startup client universe, never as persisted picker selection. |
| `usageTabEnabled` | boolean | Show the subscription quota Usage tab in the TUI. |
| `usageProviders` | string[] | Explicit allowlist of subscription providers the TUI may fetch. Empty means cache-display mode. |
| `scanner.opencodeDbPaths` | string[] | Authoritative additional current-format OpenCode SQLite database files. Missing, unreadable, or obsolete entries fail explicitly. This is the only custom OpenCode scan setting. |
| `scanner.extraScanPaths` | object | Persistent extra scan roots by client id. |

CLI flags override matching config values for a single invocation.

OpenCode is intentionally not an `extraScanPaths` client. Put each additional
current-format database file in `scanner.opencodeDbPaths`; OpenCode entries in
`scanner.extraScanPaths` or `TOKSCALE_EXTRA_DIRS` are ignored. Automatic
discovery treats only `NotFound` as absent; other discovery I/O failures are
reported explicitly.

## Environment variables

| Variable | Meaning |
| --- | --- |
| `TOKSCALE_CONFIG_DIR` | Overrides the general config/cache root used by Tokscale. Non-empty values are used verbatim. Empty values are treated as unset. |
| `TOKSCALE_EXTRA_DIRS` | One-off extra scan roots as `client:/abs/path,client:/abs/path`. |
| `TOKSCALE_USAGE_ZAI_CODING_PLAN_API_KEY` | Z.ai/Zhipu GLM Coding Plan quota key. |
| `TOKSCALE_USAGE_KIMI_CODING_PLAN_API_KEY` | Kimi Code Console quota key. |
| `TOKSCALE_USAGE_MINIMAX_TOKEN_PLAN_CN_KEY` | MiniMax CN Token Plan subscription key. |
| `TOKSCALE_USAGE_MINIMAX_TOKEN_PLAN_GLOBAL_KEY` | MiniMax Global Token Plan subscription key. |

Client-specific homes are also respected where the client supports them, such as
`CODEX_HOME`, `GEMINI_CLI_HOME`, `KIMI_CODE_HOME`, `HERMES_HOME`,
`CODEBUFF_DATA_DIR`, `GOOSE_PATH_ROOT`, and `GROK_HOME`. These client home
overrides trim leading/trailing whitespace and fall back to the default client
path when set to a blank value.

Path-like environment variables intentionally use two different policies:

- Client scan roots trim surrounding whitespace and treat blank values
  as a request to use the default root.
- Config and XDG roots (`TOKSCALE_CONFIG_DIR`, `XDG_CONFIG_HOME`, and
  `XDG_DATA_HOME`) are system/configuration boundaries. Tokscale keeps
  non-empty values verbatim rather than trimming them. Empty
  `TOKSCALE_CONFIG_DIR` is treated as unset by the config resolver; XDG
  variables are otherwise left to the platform path resolver or direct scanner
  root logic that reads them.

## Cache layout

Regenerable caches live under `${TOKSCALE_CONFIG_DIR}/cache/` or the platform
default config root. The files listed in this section can be deleted when you
want a fresh local rebuild:

- `tui-data-cache.json`
- `shards/` (scan-input message cache)
- `pricing-litellm.json`
- `pricing-openrouter.json`
- `pricing-models-dev.json`
- `fonts/`
- `images/`

Scan-input message cache writes use the v8 shard envelope and stable explicit
parser keys. Ordinary reports read and write current v8 shards without
traversing, migrating, or deleting legacy v1 through v7 shards. Run `tokscale
cache prune` when you explicitly want a full traversal that removes classified
legacy shards; there is no automatic migration.

The TUI aggregate cache is separate from scan-input message shards. Reports never
write it; use `tokscale cache warm` when you intentionally want to prebuild it.

Retired `warp-cache/` and `antigravity-cache/` integration roots are not
current Inputs. Tokscale ignores them and does not delete them automatically.
Current local Warp usage remains provider-owned in `warp.sqlite`; current AGY
CLI usage remains provider-owned under
`$GEMINI_CLI_HOME/antigravity-cli/conversations/`.

## Subscription providers

Canonical `usageProviders` ids:

```text
claude
codex
zai
amp
copilot
grok
kimi
minimax-token-plan-cn
minimax-token-plan-global
```

General-purpose provider API keys such as `ZAI_API_KEY`, `GLM_API_KEY`,
`KIMI_API_KEY`, `MINIMAX_API_KEY`, and `MINIMAX_API_TOKEN` are not used for
subscription quota lookups.

Codex subscription usage reads the currently authenticated account from
provider-owned Codex auth state (`$CODEX_HOME/auth.json`, the standard Codex
config locations, or the official macOS keychain item). Tokscale does not copy,
refresh, switch, or modify those credentials. A legacy Tokscale
`codex-credentials.json` is obsolete and ignored by current versions.
