# Configuration

Tokscale stores most local settings under the platform config directory:

- Linux/macOS default: `~/.config/tokscale/settings.json`
- Windows default: `%APPDATA%\tokscale\settings.json`
- Override root: `TOKSCALE_CONFIG_DIR`

Current exception: Cursor integration state is still stored under the user's
home directory at `$HOME/.config/tokscale/cursor-credentials.json` and
`$HOME/.config/tokscale/cursor-cache/`. Setting `TOKSCALE_CONFIG_DIR` does not
isolate Cursor credentials or Cursor usage cache today.

## Example

```json
{
  "colorPalette": "blue",
  "includeUnusedModels": false,
  "defaultClients": ["opencode", "claude"],
  "usageTabEnabled": true,
  "usageProviders": ["codex", "zai", "minimax-token-plan-cn"],
  "scanner": {
    "extraScanPaths": {
      "codex": [
        "/Users/me/workspace/project-a/.codex/sessions"
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

## Settings

| Setting | Type | Meaning |
| --- | --- | --- |
| `colorPalette` | string | TUI color theme. Known values include `green`, `halloween`, `teal`, `blue`, `pink`, `purple`, `orange`, `monochrome`, and `ylgnbu`. |
| `includeUnusedModels` | boolean | Show zero-token models in reports. |
| `autoRefreshEnabled` | boolean | Enable TUI auto-refresh for local reports. |
| `autoRefreshMs` | number | TUI auto-refresh interval in milliseconds. |
| `nativeTimeoutMs` | number | Maximum processing time for native subprocess work. |
| `defaultClients` | string[] | Client filter used when no `--client/-c` flag is passed. |
| `light.writeCache` | boolean | Allow `tokscale --light` to refresh the TUI startup cache after rendering. |
| `usageTabEnabled` | boolean | Show the subscription quota Usage tab in the TUI. |
| `usageProviders` | string[] | Explicit allowlist of subscription providers the TUI may fetch. Empty means cache-display mode. |
| `scanner.extraScanPaths` | object | Persistent extra scan roots by client id. |

CLI flags override matching config values for a single invocation.

## Environment variables

| Variable | Meaning |
| --- | --- |
| `TOKSCALE_CONFIG_DIR` | Overrides the general config/cache root used by Tokscale. It does not currently move Cursor credentials or Cursor usage cache. |
| `TOKSCALE_NATIVE_TIMEOUT_MS` | Overrides `nativeTimeoutMs`. |
| `TOKSCALE_EXTRA_DIRS` | One-off extra scan roots as `client:/abs/path,client:/abs/path`. |
| `TOKSCALE_API_TOKEN` | Tokscale hosted-service API token for non-interactive submit/delete commands. |
| `TOKSCALE_HEADLESS_DIR` | Overrides the headless capture root. |
| `TOKSCALE_USAGE_ZAI_CODING_PLAN_API_KEY` | Z.ai/Zhipu GLM Coding Plan quota key. |
| `TOKSCALE_USAGE_KIMI_CODING_PLAN_API_KEY` | Kimi Code Console quota key. |
| `TOKSCALE_USAGE_MINIMAX_TOKEN_PLAN_CN_KEY` | MiniMax CN Token Plan subscription key. |
| `TOKSCALE_USAGE_MINIMAX_TOKEN_PLAN_GLOBAL_KEY` | MiniMax Global Token Plan subscription key. |

Client-specific homes are also respected where the client supports them, such as
`CODEX_HOME`, `GEMINI_CLI_HOME`, `KIMI_CODE_HOME`, `HERMES_HOME`,
`CODEBUFF_DATA_DIR`, `GOOSE_PATH_ROOT`, and `GROK_HOME`.

## Cache layout

Regenerable caches live under `${TOKSCALE_CONFIG_DIR}/cache/` or the platform
default config root:

- `tui-data-cache.json`
- `source-message-cache.bin`
- `source-message-cache.lock`
- `pricing-litellm.json`
- `pricing-openrouter.json`
- `pricing-models-dev.json`
- `opencode-migration.json`
- `fonts/`
- `images/`

Integration sync artifacts use sibling roots:

- `antigravity-cache/`
- `trae-cache/`
- `warp-cache/`

Cursor is the current exception: its credentials and cache live at
`$HOME/.config/tokscale/cursor-credentials.json` and
`$HOME/.config/tokscale/cursor-cache/`, independent of `TOKSCALE_CONFIG_DIR`.

These caches can be deleted when you want a fresh local rebuild. Credentials are
stored separately by their integration commands and should be treated as secrets.

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
warp
```

General-purpose provider API keys such as `ZAI_API_KEY`, `GLM_API_KEY`,
`KIMI_API_KEY`, `MINIMAX_API_KEY`, and `MINIMAX_API_TOKEN` are not used for
subscription quota lookups.
