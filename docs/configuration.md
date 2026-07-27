# Configuration

Tokenx keeps all product-owned settings and regenerable state under one
cross-platform product root:

- Default: `~/.tokenx/settings.json`
- Override root: `TOKENX_CONFIG_DIR`

## Example

```json
{
  "colorPalette": "blue",
  "timeZone": "Asia/Shanghai",
  "defaultClients": ["opencode", "claude"],
  "subscription": {
    "enabled": true,
    "providers": ["codex", "zai", "minimax-token-plan-cn"]
  },
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
| `colorPalette` | string | Complete TUI semantic theme, covering surfaces, navigation, selections, metrics, status, and visualizations. Known values include `green`, `halloween`, `teal`, `blue`, `pink`, `purple`, `orange`, `monochrome`, `ylgnbu`, `graphite`, `lagoon`, and `dusk`. An explicit `--theme` overrides this saved value. |
| `autoRefreshEnabled` | boolean | Enable background TUI refresh of the fixed startup client universe. |
| `autoRefreshMs` | number | Background TUI refresh interval in milliseconds. View changes do not reset it. |
| `defaultClients` | string[] | Default scan scope when no `--client/-c` flag is passed. Reports use it for that invocation; the TUI fixes it as the startup client universe, never as persisted picker selection. |
| `timeZone` | IANA timezone string | Optional calendar authority such as `Asia/Shanghai` or `America/New_York`. When absent, Tokenx resolves the operating system's configured IANA timezone once at startup. Per-shell `TZ` is deliberately ignored. |
| `subscription.enabled` | boolean | Show the remote Subscription tab in the TUI. |
| `subscription.providers` | string[] | Explicit allowlist of subscription providers the TUI may fetch. Empty means cache-display mode. |
| `scanner.opencodeDbPaths` | string[] | Authoritative absolute paths to additional current-format OpenCode SQLite database files. Missing, unreadable, relative, or invalid entries fail explicitly. This is the only custom OpenCode scan setting. |
| `scanner.extraScanPaths` | object | Persistent absolute extra scan roots by client id. Relative paths are rejected so acquisition identity cannot depend on the process working directory. |

CLI flags override matching config values for a single invocation.

Settings are strict typed input. Unknown top-level keys and unknown keys inside
`subscription` are parse errors.
Theme names and client ids in `settings.json` use their documented canonical
lowercase spelling; unknown or differently cased strings are parse errors.
`timeZone` must be a canonical IANA timezone name; POSIX `TZ` expressions and
per-shell overrides are not configuration inputs.
TUI, models, and cache-warm commands read settings once at startup, so a file
edit during execution takes effect together on the next invocation rather than
mixing client, scanner, theme, refresh, or subscription policy from different
reads.

Client labels are defined exclusively by
`crates/tokenx-engine/client-catalog.json`. They cannot be overridden through
local settings.

OpenCode is intentionally not an `extraScanPaths` client. Put each additional
current-format database file in `scanner.opencodeDbPaths`; OpenCode entries in
`scanner.extraScanPaths` are rejected. Automatic discovery treats only
`NotFound` as absent; other discovery I/O failures are reported explicitly.

## Environment variables

| Variable | Meaning |
| --- | --- |
| `TOKENX_CONFIG_DIR` | Overrides the general config/cache root used by Tokenx. The value must be absolute. Surrounding whitespace is trimmed; empty and whitespace-only values are treated as unset. |
| `TOKENX_USAGE_ZAI_CODING_PLAN_API_KEY` | Z.ai/Zhipu GLM Coding Plan quota key. |
| `TOKENX_USAGE_KIMI_CODING_PLAN_API_KEY` | Kimi Coding Plan quota key. |
| `TOKENX_USAGE_MINIMAX_TOKEN_PLAN_CN_KEY` | MiniMax CN Token Plan subscription key. |
| `TOKENX_USAGE_MINIMAX_TOKEN_PLAN_GLOBAL_KEY` | MiniMax Global Token Plan subscription key. |

Automatic input discovery uses only the fixed client paths documented in
[`clients.md`](clients.md). `scanner.extraScanPaths` is the sole configuration
for additional recursive input roots; OpenCode uses
`scanner.opencodeDbPaths` instead.
Discovery is scoped to the current process platform and the effective
`--home`. In particular, a Tokenx process running in WSL does not implicitly
scan Windows-mounted homes; add every `/mnt/c/...` source explicitly under the
matching `scanner.extraScanPaths.<client>` key.

`TOKENX_CONFIG_DIR` changes Tokenx's own settings and cache location. It
does not change any client input location.
Conversely, `--home` changes only the home used for built-in client discovery;
it does not redirect settings, custom pricing, or caches away from the Tokenx
product root.

## Cache layout

Regenerable caches live under `${TOKENX_CONFIG_DIR}/cache/` or
`~/.tokenx/cache/`. The files listed in this section can be deleted when you
want a fresh local rebuild:

- `generation.bin`
- `shards/` (input-record cache)
- `pricing-litellm.json`
- `pricing-openrouter.json`
- `pricing-models-dev.json`
- `subscription-usage-cache.json`

Input-record cache writes use the current shard envelope and stable
explicit decoder keys. Shards are reconstructible: writes use a private
temporary file and atomic rename, without a durability barrier per input. If
the shard store becomes unavailable, the current acquisition parses
authoritative inputs without it and reports one cache diagnostic; the next
acquisition retries the store. Ordinary generation loads and
`tokenx cache prune` accept only shards in the format supported by the running
binary. Pruning explicitly traverses the shard directory and removes current
shards whose authoritative input is absent, whose path is not canonical for the
input and decoder contract, or whose source-derived decoder contract is stale.
Traversal and classification complete before deletion; an unknown, future,
truncated, malformed, undecodable, or oversized shard aborts pruning without
deleting anything.

The canonical generation cache is separate from input-record shards. It
contains exactly one immutable `Generation`: acquisition configuration, Client
universe, source fingerprint, `FrozenUsageIndex`, Sessions, `InputFootprint`, Data
Health, and pricing diagnostics. Models never writes it; use
`tokenx cache warm` when you intentionally want to prebuild the complete
all-date generation. Its authenticated envelope is streamed through a durable
atomic replacement and rejects bodies larger than 256 MiB before allocation.

## Subscription providers

Canonical `subscription.providers` ids:

```text
claude
codex
zai
grok
kimi-coding-plan-key
kimi-coding-plan-credential
minimax-token-plan-cn
minimax-token-plan-global
```

`subscription.providers` is a typed allowlist. Unknown or duplicate ids make
`settings.json` invalid; they are never ignored or silently deduplicated.

Codex subscription usage reads the currently authenticated account from
exactly `~/.codex/auth.json`. Tokenx reads only the access token and account
id required for the quota request.

Grok Build subscription usage reads exactly `~/.grok/auth.json`, requires one
usable `https://auth.x.ai::*` account entry, and queries the provider quota
backend directly. Tokenx does not invoke the Grok executable.

Kimi Coding Plan is exposed as two independent providers.
`kimi-coding-plan-key` reads only
`TOKENX_USAGE_KIMI_CODING_PLAN_API_KEY` and displays
`Kimi Coding Plan (key)`. `kimi-coding-plan-credential` reads only
`~/.kimi-code/credentials/kimi-code.json` and displays
`Kimi Coding Plan (credential)`. Its credential path is fixed.

MiniMax CN and Global are distinct subscription products. Their TUI provider
labels are `MiniMax Token Plan CN` and `MiniMax Token Plan Global`; neither
region is an account identity.

The normalized subscription cache uses schema
`tokenx.subscription-usage`, version `1`, and a five-minute freshness window.
Each output stores the canonical provider id from `subscription.providers`;
human-readable labels are derived when the output is rendered.
Wrong-schema, wrong-version, malformed, and I/O failures are explicit cache
errors; an entry older than five minutes is an ordinary miss. Neither condition
causes a remote request when `subscription.providers` is empty.
