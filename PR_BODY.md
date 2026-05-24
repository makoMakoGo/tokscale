## Summary

Adds a `tokscale usage` CLI command and TUI **Usage** tab that displays live subscription quota and remaining usage for AI coding assistants.

### Quick start

```bash
tokscale usage           # light-mode card output
tokscale usage --json    # JSON for scripting
tokscale tui             # switch to Usage tab (2nd tab)
```

## Supported Providers (7)

| Provider | Auth Source | Metrics |
|---|---|---|
| **Claude** | `~/.claude/.credentials.json` / macOS Keychain | Session (5h), Weekly (7d), Opus (7d) |
| **Codex** | `CODEX_HOME/auth.json`, `~/.config/codex/auth.json`, `~/.codex/auth.json`, macOS Keychain | Session (5h), Weekly (7d) |
| **Z.ai** | `ZAI_API_KEY` / `GLM_API_KEY` env var | Session, Weekly, Web Search |
| **Amp** | `~/.local/share/amp/secrets.json` | Free tier ($remaining/$total), Credits |
| **GitHub Copilot** | macOS Keychain `gh:github.com`, `hosts.yml` (respects `GH_CONFIG_DIR`) | Premium, Chat, Completions (paid + free) |
| **Kimi Code** | `~/.kimi/credentials/kimi-code.json` | Session, Weekly |
| **MiniMax** | `MINIMAX_API_KEY` / `MINIMAX_API_TOKEN` env var | Session (prompts) |

Only providers with valid credentials are queried — the rest are silently skipped.

## Architecture

Refactored `commands/usage.rs` into a `commands/usage/` module directory:

```
commands/usage/
├── mod.rs          # Shared types, fetch_all(), disk cache, CLI rendering
├── helpers.rs      # capitalize(), format_reset_time(), read_keychain(), render_ascii_bar()
├── claude.rs       # Claude OAuth provider
├── codex.rs        # Codex/OpenAI provider
├── zai.rs          # Z.ai provider
├── amp.rs          # Amp provider
├── copilot.rs      # GitHub Copilot provider
├── kimi.rs         # Kimi Code provider
└── minimax.rs      # MiniMax provider
```

Each provider exports `has_credentials() -> bool` and `fetch() -> Result<UsageOutput>`.

## Performance

- **Credential pre-check**: Fast local file/env checks skip providers without credentials entirely (no network calls)
- **Parallel fetching**: Active providers run concurrently via `std::thread::scope`
- **Disk cache**: Data cached to `~/.cache/tokscale/subscription-usage-cache.json` with 5-minute TTL — the Usage tab loads instantly on startup like other tabs
- **No new dependencies**: Uses only crates already in the workspace (`reqwest`, `serde`, `serde_json`, `chrono`, `anyhow`, `dirs`, `tokio`)

## Bug fixes included

- **MiniMax**: `current_interval_usage_count` is a remaining count despite its name — now handled correctly with `current_interval_used_count` preferred when available
- **Kimi**: OAuth refresh tokens (which rotate) are now persisted back to disk after each refresh, preventing stale tokens on next run
- **Codex**: Credential lookup now requires `tokens.access_token` to be present (not just the `tokens` object), supports `CODEX_HOME` env var and macOS Keychain fallback
- **Copilot**: Respects `GH_CONFIG_DIR` env var for hosts.yml path; YAML parser correctly handles other fields appearing before `oauth_token` under `github.com:`
- **OAuth payloads**: Kimi and Codex refresh payloads use `reqwest .form()` for proper URL encoding of tokens that may contain reserved characters
- **Platform compat**: `read_keychain()` returns a clean error on non-macOS instead of spawning a missing binary
- **TUI**: Empty usage state shows a proper message instead of the loading prompt; tab navigation tests updated for 7-tab layout
- **Z.ai**: Metrics ordered as Session → Weekly → Web Search regardless of API response order; renamed Monthly→Weekly, Web Searches→Web Search

## Files changed

- **New**: `commands/usage/` directory (9 provider + helper files, ~1700 lines)
- **New**: `tui/ui/usage.rs` (TUI rendering for Usage tab)
- **Modified**: `tui/app.rs` (Usage tab, disk cache, updated tab tests)
- **Modified**: `main.rs` (Usage subcommand + --home rejection)
- **Modified**: `README.md` (provider docs, corrected tab count)
- **Unchanged**: `Cargo.lock` (no new dependencies)

## Test plan

- [x] `cargo build --release` compiles cleanly with no warnings
- [x] All 451 unit tests pass (including updated tab navigation tests)
- [x] `tokscale usage --json` outputs valid JSON
- [x] `tokscale usage` renders light-mode cards with aligned columns
- [x] TUI Usage tab renders quota bars with cache + live refresh
- [x] Switching tabs is instant after first fetch (disk cache works)
- [x] Providers without credentials are silently skipped (no error spam)
- [x] `tokscale --home /tmp usage` produces clear error

🤖 Generated with [Claude Code](https://claude.com/claude-code)

Co-Authored-By: Claude Opus 4.7 <noreply@anthropic.com>
