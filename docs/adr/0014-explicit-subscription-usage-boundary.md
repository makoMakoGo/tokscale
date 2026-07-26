# ADR 0014: Subscription Usage and credential boundary

Status: Accepted

## Context

Local usage accounts for provider-owned transcripts and databases.
Subscription Usage is a separate remote product surface for account plans,
allowances, reset windows, and remaining quota. A local generation refresh is not
consent to contact an account service, and Tokenx is an analytics client
rather than an authentication authority.

## Decision

### Product and identity model

Subscription is a crate-level bounded context, separate from local generation state.
It consists of provider-specific quota adapters, a normalized short-lived
cache, a fetch service, and one provider/account/plan/metric model consumed by
the optional TUI Subscription tab.

Local usage and Subscription Usage have independent acquisition lifecycles.
Subscription data never enters local token totals, Group By, Sessions, or local
Data Health.

Each normalized output contains:

- provider display identity;
- optional stable account id, account label, and active-account flag;
- optional plan and account email; and
- zero or more metrics with a label, used and remaining percentages, optional
  remaining label, and optional reset time.

The renderer uses that identity directly. Provider and account failures are
isolated, so healthy outputs remain visible alongside explicit errors.

The complete `subscription.providers` id set is:

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

Settings accept these exact typed ids. Unknown or duplicate ids make settings
loading or validation fail with an explicit diagnostic.

### Remote-request consent

The TUI may contact a provider only when `subscription.enabled` is true,
`subscription.providers` contains that provider, and its exact subscription
surface has usable credentials. An empty provider list is cache-display mode:
the tab may show a fresh normalized cache but sends no remote request.

The TUI lifecycle is:

- entering Subscription starts at most one automatic fetch in a TUI session;
- `u` explicitly starts a Subscription Usage fetch;
- Subscription accepts only subscription actions plus shell navigation, theme,
  and quit actions;
- local-usage `r`, `R`, `+`, `-`, and `e` actions are unavailable while Subscription
  is active; and
- Subscription Usage is never polled in the background.

Fetch state is one closed lifecycle:
`NotStarted -> Queued -> Running -> Settled`. Initial tab entry requests only
from `NotStarted`; therefore an earlier manual request, including one completed
before the first tab entry, consumes the initial-fetch opportunity. Repeated
requests while queued or running are rejected as already in progress.

Subscription content, footer, and contextual actions consume one independent
`SubscriptionPresentation`: cold fetching without an installed result, prompt,
empty, or attributed results with an optional refresh in progress. A cold fetch
uses its own elapsed timer and centered activity footer. Existing results remain
visible during a warm fetch. The ordinary Subscription footer summarizes only
subscription providers, limits, and provider errors; it never reads local
tokens, cost, generation cache warnings, pricing status, or local refresh
state. Local and subscription presentations may reuse stateless renderers, but
they do not share lifecycle state.

Each configured provider has one request entry point: it reads its credentials
once, performs the fetch, and returns a provider-attributed result or explicit
error. There is no availability preflight. A provider without usable
credentials therefore produces its concrete credential error rather than
ordinary empty data or a second credential lookup.

### Credential authority

The provider application, provider CLI, OS credential store, or a
purpose-specific environment variable owns authentication. Tokenx's
credential authority is limited to reading the fields required for an explicit
quota request. It does not own login, logout, account switching, OAuth refresh,
or credential persistence.

Codex quota lookup reads exactly `~/.codex/auth.json`. Only the access token
and account id required by the request are consumed.

Grok Build quota lookup reads exactly `~/.grok/auth.json`. The file must contain
exactly one usable `https://auth.x.ai::*` account entry; absence and ambiguity
are explicit provider errors. Tokenx reads only the access key and the
provider-owned principal, user-facing name, and email fields required to issue
the quota request and identify its result. It queries the Grok Build
subscription backend directly and never invokes the Grok executable. The
provider-owned principal is the account id.

Purpose-specific subscription credentials are:

- Z.ai/Zhipu GLM Coding Plan:
  `TOKENX_USAGE_ZAI_CODING_PLAN_API_KEY`;
- Kimi Coding Plan (key):
  `TOKENX_USAGE_KIMI_CODING_PLAN_API_KEY`;
- Kimi Coding Plan (credential):
  exactly `~/.kimi-code/credentials/kimi-code.json`;
- MiniMax CN Token Plan:
  `TOKENX_USAGE_MINIMAX_TOKEN_PLAN_CN_KEY`; and
- MiniMax Global Token Plan:
  `TOKENX_USAGE_MINIMAX_TOKEN_PLAN_GLOBAL_KEY`.

General provider API keys are not subscription-plan credentials. Claude, Grok,
Kimi Coding Plan (credential), and Codex adapters read only their
provider-owned current authentication artifacts. The Kimi key and credential
providers are independent and never substitute for one another. Missing,
ambiguous, expired, or rejected authentication is an explicit provider error
repaired with provider tooling.

MiniMax Token Plan CN and MiniMax Token Plan Global are separate subscription
surfaces with the display identities `MiniMax Token Plan CN` and
`MiniMax Token Plan Global`. Their region is not an account identity. Unless
the provider returns a real account id, both outputs carry no `UsageAccount`.

### Normalized cache

`subscription-usage-cache.json` is credential-free derived state with this
closed envelope:

- schema id `tokenx.subscription-usage`;
- version `2`;
- a Unix-seconds storage timestamp; and
- normalized `SubscriptionOutput` data whose provider is the canonical
  `ProviderId`, serialized with the same stable id accepted by
  `subscription.providers`.

Provider fetchers return identity-neutral quota payloads. The exhaustive
service dispatch attaches the requested `ProviderId` to each successful
payload, and presentation derives the human-readable provider label from that
typed identity. Provider parsers therefore cannot create a second display-name
identity that diverges from settings, cache, or dispatch.

The envelope and nested normalized types reject unknown fields. Wrong schema or
version, malformed data, and cache I/O failures are explicit Subscription-tab cache
errors. Entries older than 300 seconds are ordinary misses.

Cache-display mode never converts a miss into a remote request. A fetch with
one or more healthy outputs atomically replaces the complete installed
in-memory snapshot, even when other providers failed; those failures remain
visible beside the new snapshot. An empty or wholly failed fetch keeps the
installed snapshot and disk cache while exposing the new errors.

Provider-attributed errors are Subscription Usage results even when every
provider failed. They remain visible in the Subscription content and result summary
instead of collapsing into a generic failure page.

The in-memory installation and disk publication are separate atomic
boundaries. Disk publication uses a temporary file and rename. A disk write
failure retains the newly installed in-memory snapshot and adds an explicit
cache diagnostic. Cache state contains no access token, refresh token, cookie,
API key, raw authentication response, or raw provider response.

## Consequences

Local refresh cannot unexpectedly contact a remote account. The provider
adapters and TUI renderer share one provider/account/plan identity, partial
failures remain visible, and cached quota output carries no authentication
material. Every authentication mutation stays under provider authority.
