# Cline SDK v1 local-session facts

Last verified: 2026-07-20

Verification used `cline/cline` commit
[`c2faf38d7283e79c473799d973357e50bd76e2fb`](https://github.com/cline/cline/commit/c2faf38d7283e79c473799d973357e50bd76e2fb),
Cline VS Code `4.0.0`, Cline CLI `3.0.46`, and a read-only sanitized
snapshot of a local Cline CLI corpus. No prompts, credentials, or message
content were inspected.

## Shared storage

VS Code 4.0+ and CLI 3.x use the same SDK v1 session artifacts. Tokenx reads
the fixed `~/.cline/data/sessions` root; additional roots are configured with
`scanner.extraScanPaths.cline`.

The layout is:

```text
~/.cline/data/
+-- db/
|   `-- sessions.db
`-- sessions/
    `-- <root-session-id>/
        +-- <root-session-id>.json
        +-- <root-session-id>.messages.json
        +-- <agent-id>.messages.json
        `-- <agent-id>__<team-task-id>.messages.json
```

`sessions.db` is an index. The root JSON file is a manifest containing fields
such as `version`, `session_id`, `workspace_root`, and artifact paths. Message
artifacts are the authoritative usage input.

The root messages file is the lead transcript. A child stem containing only an
agent ID is a subagent artifact. A child stem containing
`<agent-id>__<team-task-id>` is a teammate artifact. Every artifact retains the
root session ID in its envelope; `agent` distinguishes `lead`, `subagent`, and
`teammate`.

## Messages v1 usage shape

The artifact envelope is:

```text
version: 1
updated_at: ISO timestamp
agent: lead | subagent | teammate
sessionId: root session ID
messages: array
```

Usage is stamped per completed assistant turn. A usage-bearing assistant
message contains:

```text
ts: epoch milliseconds
modelInfo.id
modelInfo.provider
metrics.inputTokens
metrics.outputTokens
metrics.cacheReadTokens
metrics.cacheWriteTokens
metrics.cost (vendor field; may be absent)
```

`inputTokens` is cache-inclusive. Exclusive input is therefore
`max(inputTokens - cacheReadTokens - cacheWriteTokens, 0)`.

The SDK contract includes vendor `cost`, but a verified Laguna M.1 free-model
artifact omitted it; vendor cost is not a Tokenx usage input fact.

## Verified local observation

The sanitized CLI snapshot contained one root session and 12 assistant turns
with metrics. Those turns used model `poolside/laguna-m.1:free` and provider
`cline`, with non-zero token usage.

## Evidence index

- `sdk/packages/shared/src/storage/paths.ts` -- default storage layout;
- `sdk/packages/core/src/services/session-artifacts.ts` -- artifact layout and
  child file stems;
- `sdk/packages/core/src/services/session-data.ts` -- v1 envelope and per-turn
  metrics persistence;
- `sdk/packages/core/src/services/storage/sqlite-session-store.ts` -- SQLite
  index location;
- `apps/cli/src/tests/headless/per-turn-metrics.live.test.ts` -- per-turn usage
  semantics.
