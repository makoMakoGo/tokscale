# ADR 0014: Subscription Usage and credential boundary

Status: Accepted

## Context

Local reports account for provider-owned transcripts and databases.
Subscription Usage is a separate remote product surface for account plans,
allowances, reset windows, and remaining quota. A local report refresh is not
consent to contact an account service, and Tokscale is an analytics client
rather than an authentication authority.

## Decision

### Product and identity model

Subscription Usage consists of `tokscale usage`, the optional TUI Usage tab,
provider-specific quota adapters, a normalized short-lived cache, and one
provider/account/plan/metric model shared by CLI and TUI.

Local reports and Subscription Usage have independent acquisition lifecycles.
Subscription data never enters local token totals, Group By, Sessions, or local
Data Health.

Each normalized output contains:

- provider display identity;
- optional stable account id, account label, and active-account flag;
- optional plan and account email; and
- zero or more metrics with a label, used and remaining percentages, optional
  remaining label, and optional reset time.

Both renderers use that identity directly. Provider and account failures are
isolated, so healthy outputs remain visible alongside explicit errors.

The complete `usageProviders` id set is:

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

Settings accept these exact ids. Unknown ids are ignored, and duplicates
collapse in first-seen order.

### Remote-request consent

The TUI may contact a provider only when `usageTabEnabled` is true,
`usageProviders` contains that provider, and its exact subscription surface has
usable credentials. An empty provider list is cache-display mode: the tab may
show a fresh normalized cache but sends no remote request.

The TUI lifecycle is:

- entering Usage starts at most one automatic fetch in a TUI session;
- `u` explicitly starts a Subscription Usage fetch;
- `r` refreshes local reports only;
- `R` controls local-report automatic refresh only; and
- Subscription Usage is never polled in the background.

`tokscale usage` is explicit remote-request consent. It detects providers with
usable subscription credentials, fetches them concurrently, renders every
healthy output, and reports every provider failure. A partial failure makes the
command fail after available output is rendered.

An explicitly configured TUI provider without usable credentials produces a
provider error rather than ordinary empty data.

### Credential authority

The provider application, provider CLI, OS credential store, or a
purpose-specific environment variable owns authentication. Tokscale's
credential authority is limited to reading the fields required for an explicit
quota request. It does not own login, logout, account switching, OAuth refresh,
or credential persistence.

Codex quota lookup reads the active provider-owned authentication artifact in
this order:

1. `$CODEX_HOME/auth.json` when `CODEX_HOME` is set;
2. `~/.config/codex/auth.json`;
3. `~/.codex/auth.json`;
4. the provider's macOS Keychain entry.

Only the access token and account id required by the request are consumed.

Purpose-specific subscription credentials are:

- Z.ai/Zhipu GLM Coding Plan:
  `TOKSCALE_USAGE_ZAI_CODING_PLAN_API_KEY`;
- Kimi Code:
  `TOKSCALE_USAGE_KIMI_CODING_PLAN_API_KEY` or provider-owned Kimi Code OAuth;
- MiniMax CN Token Plan:
  `TOKSCALE_USAGE_MINIMAX_TOKEN_PLAN_CN_KEY`; and
- MiniMax Global Token Plan:
  `TOKSCALE_USAGE_MINIMAX_TOKEN_PLAN_GLOBAL_KEY`.

General provider API keys are not subscription-plan credentials. Claude, Amp,
Copilot, Grok, Kimi, and Codex adapters read only their provider-owned current
authentication artifacts. Missing, ambiguous, expired, or rejected
authentication is an explicit provider error repaired with provider tooling.

### Normalized cache

`subscription-usage-cache.json` is credential-free derived state with this
closed envelope:

- schema id `tokscale.subscription-usage`;
- version `1`;
- a Unix-seconds storage timestamp; and
- normalized `UsageOutput` data.

The envelope and nested normalized types reject unknown fields. Wrong schema or
version, malformed data, and cache I/O failures are explicit cache errors.
Entries older than 300 seconds are ordinary misses.

Cache-display mode never converts a miss into a remote request. A fetch that
returns normalized output atomically replaces the installed snapshot and
cache. An empty or failed fetch keeps the installed snapshot and exposes its
errors. Cache state contains no access token, refresh token, cookie, API key,
raw authentication response, or raw provider response.

## Consequences

Local refresh cannot unexpectedly contact a remote account. CLI and TUI share
one provider/account/plan identity, partial failures remain visible, and cached
quota output carries no authentication material. Every authentication mutation
stays under provider authority.
