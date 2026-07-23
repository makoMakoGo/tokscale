# ADR 0006: Agent identity for the Agents tab

Status: Accepted

Related PR: #50

## Context

Several local clients persist runtime labels that look like agent names but are
not stable reporting identities. Codex nicknames, Claude temporary sidechain
names, Kimi path segments such as `main` or `agent-0`, and similar display or
instance labels can create many misleading rows in the TUI `Agents` tab.

The `Agents` tab is an accounting view. Its primary grouping key must represent
a stable agent type or role, not a per-run presentation label.

## Decision

An `Agents` row has the structured identity `(Client, Agent)`, where `Agent`
is the stable agent type or role produced by that Client's parser.

- Runtime nicknames, path segments, and one-off generated names must not be used
  as the primary aggregation key.
- Provider is usage attribution and is not part of Agent identity.
- The same Agent name within one Client is one row. The same Agent name emitted
  by different Clients remains separate rows; aggregation must never merge it
  across Clients.
- Instance identifiers belong in `agent_instance` and may contribute to the
  `Instances` count.
- Codex uses stable role, subagent, or exec-session labels; `agent_nickname` is not
  a grouping identity.
- Claude preserves known stable subagent types and collapses unknown temporary
  sidechain names to `Claude Subagent`.
- OMP recovers task agent roles from parent `task` calls. Canonical
  `.swarm_<swarm>/context/swarm-<swarm>-<agent>-<iteration>.jsonl` artifacts
  share the stable `OMP Swarm` reporting identity, while the full artifact stem
  remains the instance identifier.
- Kimi uses explicit `config.update.profileName` values from the known profile
  set only; filesystem segments such as `main` and `agent-N` are not fallbacks.
- Messages without a recognized stable agent identity should not create an
  `Agents` row.

Each Client parser owns any Client-specific Agent extraction and
normalization. The value written to `UnifiedMessage.agent` is authoritative;
the aggregate and TUI cache preserve it without another alias table, case
normalization, comma-joined Client set, or cross-Client reconciliation.

Changing parsed Agent semantics must bump the affected parser revision, and
changing its persisted TUI shape must bump the TUI cache schema, so stale
labels or identities are rebuilt.

## Consequences

Client parsers may keep Client-specific recovery logic, but the value written
to `UnifiedMessage.agent` must already be a stable reporting identity.
Aggregation and cache code do not reinterpret runtime labels after parsing.
The public Agent DTO carries one `client` field because every row represents
exactly one `(Client, Agent)` identity.

New local client support must identify its stable agent field before populating
`UnifiedMessage.agent`. If no stable field exists, leave the agent unset instead
of deriving one from a display name or path.
