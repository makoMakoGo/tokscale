# ADR 0024: Subscription Usage product boundary and redesign

## Status

Accepted foundation. The provider, account, plan, cache, and presentation
details remain in design under issue #146.

## Context

Tokscale has two different data products that accumulated overlapping names
and responsibilities:

- Local reports parse provider-owned transcripts, databases, and session
  files to account for token usage.
- Subscription Usage contacts provider APIs to display account-level quota,
  limits, reset windows, and plan state.

Subscription Usage also accumulated credential copying, account switching,
provider-specific sync commands, inconsistent caches, and presentation models
that did not share one account or plan identity. Cursor and Trae expanded this
surface further by mixing local parsing with Tokscale-owned credentials and
remote synchronization. Treating each removal as an isolated integration
cleanup would leave the subsystem without a durable product boundary.

This ADR records that boundary and the accepted direction for the whole Usage
subsystem. It does not pretend that the final provider/account/plan model is
already designed.

## Scope

Subscription Usage includes:

- the `tokscale usage` report;
- the optional TUI Usage tab;
- remote subscription and coding-plan provider adapters;
- discovery of externally owned credentials;
- normalized quota-result caching;
- provider, account, and plan identity presented by the CLI and TUI.

Local transcript and session reports remain a separate product surface. They
are affected only where an old integration blurred the boundary between local
parsing, remote quota lookup, and account management.

## Decision

### Stable product boundaries

- Local report refresh never performs a remote Subscription Usage request.
  Remote access remains explicit and bounded by ADR 0014.
- Tokscale is a credential consumer, not an account manager, as established by
  ADR 0023. It does not copy provider credentials, log users in or out, switch
  accounts, refresh OAuth credentials, or rewrite provider authentication.
- Subscription credentials remain in provider-owned files, OS credential
  stores, or explicitly named environment variables. Tokscale settings and
  caches must not contain secret values.
- A cache may persist only a normalized quota result and its freshness
  metadata. It must not persist access tokens, refresh tokens, session cookies,
  API keys, or raw authentication responses.
- Provider and account failures are isolated. One unavailable or malformed
  provider result must not hide healthy Usage data from other providers or
  accounts.
- CLI and TUI presentations must consume one provider/account/plan domain
  model. Rendering code must not invent a second identity scheme.

### Cursor and Trae removal

Cursor and Trae are removed from the maintained product surface as the first
application of this boundary:

- Remove them from the client catalog, local scan definitions, adapters,
  parsers, reports, TUI, Wrapped, CLI namespaces, assets, tests, and user
  documentation.
- `cursor` and `trae` are invalid client ids. Tokscale does not discover their
  old cache directories, copy their credentials, refresh tokens, or contact
  either service.
- Persisted parser discriminants remain only as retired cache-format tags
  until the next cache-format break. No active adapter may request them.
- Legacy Tokscale-owned files are ignored rather than deleted automatically;
  deleting user files is an explicit maintenance action.

Codex and ChatGPT subscription lookup reads the active provider-owned Codex
authentication artifact under ADR 0023. It does not restore Tokscale account
switching or a second credential store.

ADR 0033 applies the same boundary to Warp: Tokscale no longer stores Warp
credentials, contacts its GraphQL quota surface, maintains a synchronized quota
cache, or exposes a `tokscale warp ...` namespace. This does not remove the
separate local Warp Client that reads provider-owned `warp.sqlite`.

The existing `TOKSCALE_USAGE_ZAI_CODING_PLAN_API_KEY` contract remains valid.
Future support for multiple Z.ai plans must reference multiple external
secrets rather than copy their values into Tokscale.

## Design still in progress

Issue #146 owns the detailed redesign, including:

- the provider/account/plan identity and ordering model;
- which existing providers remain supported;
- multiple external-secret references and deduplication;
- normalized cache schema and fresh, stale, and unavailable semantics;
- refresh scheduling and the optional Usage-tab lifecycle;
- CLI JSON, TUI rendering, redaction, and partial-failure presentation.

Those decisions must preserve the boundaries above. They may refine or
supersede this ADR once the complete model is known; they must not reintroduce
Tokscale-owned secrets or account-management commands as incidental provider
features.

## Relationship to earlier decisions

This ADR extends ADR 0014's explicit remote-access boundary and ADR 0023's
provider-owned credential policy. It supersedes the Cursor and Trae clauses in
ADR 0015, ADR 0018, and ADR 0023, and removes the `parse_local` capability split
from ADR 0007 because every remaining catalog identity is an ordinary local
report input.

## Consequences

The fork currently has no Cursor or Trae local reporting, remote Usage,
account management, sync command, or TUI presence. Existing commands and
settings that name them fail as invalid usage instead of producing an empty
success.

The Usage subsystem is intentionally an active redesign area rather than a
finished provider list frozen by this ADR. New Usage work must first identify
credential ownership, remote-request consent, cache contents, failure
isolation, and provider/account/plan identity before adding presentation or
convenience commands.
