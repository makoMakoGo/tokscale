# Kimi Code local-session facts

Last verified: 2026-07-18

This document records observed upstream storage facts, not Tokenx policy. The
corresponding ingestion decision lives in
[ADR 0001](../adr/0001-no-silent-fallback.md). Verification
used `MoonshotAI/kimi-code` at commit
`3086e4703992fbbe7a41379405ee243713ad9ced` and a read-only snapshot of a real
local `~/.kimi-code` corpus. No credentials, prompts, message content, or tool
schemas were inspected or copied.

## Supported storage boundary

Current Kimi Code sessions use one ordered wire per agent:

```text
~/.kimi-code/
+-- session_index.jsonl
`-- sessions/
    `-- wd_<slug>_<workdir-sha256-prefix>/
        `-- session_<uuid>/
            +-- state.json
            +-- logs/kimi-code.log
            `-- agents/
                +-- main/wire.jsonl
                `-- agent-N/wire.jsonl
```

`session_index.jsonl` is append-only. A normal entry contains `sessionId`, an
absolute `sessionDir`, and `workDir`; deletion is represented by a later
`{"sessionId":"...","deleted":true}` tombstone. `state.json` contains session
metadata and the agent tree, but it is not an ordered model-identity authority. In
the verified corpus its top-level fields contained title/workdir/time/agent
metadata and no model field.

The older Kimi CLI root-level `wire.jsonl`/`context.jsonl` format is outside
Tokenx's supported input boundary. Kimi Code still has read/migration code for
it, but current sessions write `agents/<agent-id>/wire.jsonl`; the verified
corpus contained no root-level session wire.

## Wire records that establish model identity

`wire.jsonl` is JSONL. Its first record is metadata, and live writes append
ordered records in batches. Reads tolerate a torn final line but treat malformed
interior lines as corruption. Flushes sync a batch rather than every individual
record. Resume migrations and turn-limited forks may rewrite a wire; therefore
metadata version and file creation time are not reliable feature flags.

The three relevant record shapes are:

```json
{"type":"usage.record","model":"kimi-k3","usage":{"inputOther":13954,"output":565,"inputCacheRead":12544,"inputCacheCreation":0},"usageScope":"turn","time":1784356452522}
{"type":"llm.request","kind":"loop","provider":"anthropic","model":"k3","modelAlias":"kimi-k3","time":1784356442541}
{"type":"config.update","modelAlias":"kimi-k3","time":1784356400000}
```

The example values are local structural observations with content fields
removed. Their meanings differ:

| Field | Persisted meaning |
|---|---|
| `usage.record.model` | Configured model alias used for token accounting |
| `llm.request.modelAlias` | The same alias at request time |
| `llm.request.model` | Physical model ID used by that outbound attempt |
| `llm.request.provider` | Kosong transport/protocol adapter, not the configured provider key |
| `config.update.modelAlias` | Ordered alias state change, not an alias-to-model snapshot |

Observed transport names include `kimi`, `anthropic`, `openai`,
`openai-responses`, and `google_genai`. A transport name must not be treated as
model ownership: an Anthropic-compatible request in the verified corpus carried
physical model `k3`, while an OpenAI-compatible route carried `glm-5.2`.

## Request/usage causality

Kimi Code enqueues `llm.request` before invoking the provider attempt. It writes
`usage.record` only after a successful response produces usage. Retries, strict
or media-degraded resends, and compaction rounds may emit several requests for
one eventual usage row; failed attempts may have no usage row at all. The trace
therefore establishes ordered identity state, not a one-to-one request/usage
join and not proof that an HTTP server received the request.

Within one agent wire, the safe causal relationship is:

```text
llm.request(modelAlias=A, model=M)
        |
        +-- zero or more retry/request records
        |
        `-- later usage.record(model=A)  => physical model M from the latest
                                            preceding valid request for A
```

A later request must never backfill an earlier usage row. Alias mappings are
agent-local and ordered; the same alias may resolve to a different physical
model later in the same wire.

## Introduction boundary

`llm.request` was introduced by Kimi Code PR #1448, commit
[`65d30177adc11a56bdbbe9fbc3c4b92f96efd6bb`](https://github.com/MoonshotAI/kimi-code/commit/65d30177adc11a56bdbbe9fbc3c4b92f96efd6bb),
dated 2026-07-07 14:09:19 +08:00. The commit did not bump the wire protocol:
both its parent and the commit itself used protocol `1.4`. Kimi's migration
rules also rewrite old metadata to the current version without inventing
historical request traces.

Consequently, `protocol_version == "1.4"` does not imply that a wire contains
`llm.request`. Presence of the record itself is the only valid detector. A
resumed session can also contain an old prefix without request traces and a new
suffix with them.

## Alias configuration and environment-only models

An alias is the key under `[models.<alias>]`. The default v1 engine requires
`provider`, `model`, and positive `max_context_size` values for each configured
alias; `display_name` is optional.

The current source also contains the v2 engine. In addition to named providers,
v2 accepts a flat model with an inline endpoint and no `provider`:

```toml
[models.private-alias]
model = "private-model-id"
base_url = "https://example.test/v1"
protocol = "openai_responses"
max_context_size = 128000
```

For a normal v2 request, `llm.request` is dispatched before the provider attempt
and `usage.record` only after a successful response. The request therefore
supplies the physical model ID without consulting `config.toml`. A providerless
current config can affect Tokenx's config enrichment only when the same alias
appears in an older wire prefix that has no preceding request trace. This mixed
history is valid but was not present in the verified local corpus: all 21
current model entries had a provider.

OAuth login provisions a `managed:kimi-code` provider, managed model aliases,
and `default_model` in `config.toml`. The environment-only path instead creates
these reserved runtime entries:

```text
provider key: __kimi_env__
model alias:  __kimi_env_model__
model id:     KIMI_MODEL_NAME
```

`KIMI_MODEL_NAME` requires `KIMI_MODEL_API_KEY`. The synthetic provider, alias,
default, and thinking overrides are stripped before `config.toml` is written,
but their non-secret identity still appears in wire records:

```json
{"type":"llm.request","provider":"kimi","model":"<KIMI_MODEL_NAME>","modelAlias":"__kimi_env_model__"}
{"type":"usage.record","model":"__kimi_env_model__","usage":{}}
```

With neither a configured/default model nor the environment model, headless
execution fails with “No model configured”; the v2 profile domain uses the
stable code `model.not_configured`.

## Verified local corpus snapshot

The read-only snapshot captured at 2026-07-18 15:33:05 +08:00 contained:

| Observation | Count |
|---|---:|
| Sessions / main-agent wires | 39 / 39 |
| Subagent wires | 91 |
| All per-agent wires | 130 |
| Wires containing at least one `llm.request` | 74 |
| Wires containing no `llm.request` | 56 |
| First metadata version `1.4` / `1.3` | 125 / 5 |
| Version `1.4` wires with no `llm.request` | 51 |
| `llm.request` records | 1,944 |
| `usage.record` records | 3,324 |

All 1,944 observed request records had `modelAlias`. Their complete tuple
distribution was:

| Transport | Physical model | Alias | Requests |
|---|---|---|---:|
| `anthropic` | `k3` | `kimi-k3` | 904 |
| `openai-responses` | `gpt-5.6-sol` | `openai-pro/gpt-5.6-sol` | 836 |
| `openai-responses` | `gpt-5.6-luna` | `openai-pro/gpt-5.6-luna` | 111 |
| `openai-responses` | `gpt-5.6-terra` | `openai-pro/gpt-5.6-terra` | 61 |
| `openai` | `glm-5.2` | `zai-coding-plan/glm-5.2` | 32 |

Of the usage rows, 1,407 occurred after at least one request in their wire and
all 1,407 aliases matched the latest preceding request alias. Another 24 usage
rows occurred in the old prefix of wires that began recording requests only
after a later resume. Separately, 20 version `1.4` wires with no request trace
contained 1,266 rows under alias `kimi-k2.7-code`, of which 1,265 had positive
tokens, while the current `config.toml` had no such alias. This is direct
evidence that protocol version and current config cannot replace ordered wire
evidence.

## Evidence index

The primary upstream evidence is:

- `packages/agent-core/src/agent/records/types.ts` — record contract;
- `packages/agent-core/src/agent/index.ts` and
  `packages/agent-core/src/agent/turn/index.ts` — request-before-attempt and
  success-before-usage ordering;
- `packages/agent-core/src/agent/config/index.ts` — `model` getter returns the
  alias;
- `packages/agent-core/src/agent/records/migration/` — protocol and rewrite
  rules;
- `packages/agent-core/src/session/store/` — layout, index, fork, and legacy
  boundaries;
- `packages/agent-core/src/config/schema.ts` and `config/env-model.ts` — alias
  schema and environment-only runtime entries;
- `packages/agent-core-v2/src/app/model/model.ts` and
  `modelResolverService.ts` — v2 named-provider and providerless flat-model
  contracts;
- `packages/agent-core-v2/src/agent/llmRequester/llmRequesterService.ts` — v2
  request-before-usage ordering;
- `packages/kosong/src/providers/` — transport names.

Facts about upstream behavior must be reverified when these contracts change;
the local counts above are a dated corpus observation, not a product invariant.
