# ADR 0014: Explicit Subscription Usage Boundary

## Status

Accepted.

## Context

Tokscale local reports read local transcripts and caches. Subscription usage lookups are different: they contact provider APIs with credentials and can reveal account-level plan state. Treating discovered auth files or general provider API keys as implicit permission to make remote quota requests makes the TUI lifecycle too surprising.

The provider products also have different credential boundaries. Z.ai usage here means GLM Coding Plan quota, not general Z.ai API balance. Kimi usage means Kimi Code membership quota, through Kimi Code OAuth or a Kimi Code Console API key, not the Kimi Open Platform pay-as-you-go API. MiniMax Token Plan is split between CN and Global subscription keys, and those keys are not interchangeable.

## Decision

The TUI may fetch remote subscription usage only when all of these are true:

- `usageTabEnabled` is true.
- `usageProviders` is non-empty.
- The selected provider has the credential for that subscription/coding-plan surface.

An empty `usageProviders` list means cache-display mode: the Usage tab may render cached subscription data, but it must not send remote quota requests.

The TUI fetch model is explicit and bounded:

- Entering the Usage tab may start at most one automatic subscription fetch per TUI session.
- Pressing `u` in the Usage tab explicitly refreshes subscription usage.
- Pressing `r` refreshes local reports only.
- Pressing `R` toggles local-report auto-refresh only.
- The TUI does not poll subscription usage in the background.

The `tokscale usage` CLI command is the exception. Running that command is explicit user intent, so it may auto-detect all providers with usable subscription credentials.

## Credential Policy

Tokscale accepts purpose-specific subscription credentials for plan quota lookups:

- Z.ai/Zhipu GLM Coding Plan: `TOKSCALE_USAGE_ZAI_CODING_PLAN_API_KEY`.
- Kimi Code: `TOKSCALE_USAGE_KIMI_CODING_PLAN_API_KEY` or Kimi Code OAuth credentials.
- MiniMax CN Token Plan: `TOKSCALE_USAGE_MINIMAX_TOKEN_PLAN_CN_KEY`.
- MiniMax Global Token Plan: `TOKSCALE_USAGE_MINIMAX_TOKEN_PLAN_GLOBAL_KEY`.

General provider API keys are intentionally ignored for these quota lookups. Examples include `ZAI_API_KEY`, `GLM_API_KEY`, `KIMI_API_KEY`, `MINIMAX_API_KEY`, and `MINIMAX_API_TOKEN`.

## Configuration Policy

Canonical TUI provider IDs are:

`claude`, `codex`, `zai`, `amp`, `copilot`, `grok`, `kimi`, `minimax-token-plan-cn`, `minimax-token-plan-global`, `warp`.

Unknown provider IDs are ignored while parsing settings. Explicitly selected providers without credentials produce provider errors instead of silently collapsing into "no data".

## Consequences

Users who want remote subscription usage in the TUI must configure `usageProviders` and the matching subscription credential. Existing cached Usage data can still be displayed without remote requests. The default behavior is more conservative, and local report refreshes cannot unexpectedly send remote quota requests.
