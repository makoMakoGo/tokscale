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

Group `Agents` rows by stable agent identity only.

- Runtime nicknames, path segments, and one-off generated names must not be used
  as the primary aggregation key.
- Instance identifiers belong in `agent_instance` and may contribute to the
  `Instances` count.
- Codex uses stable role, subagent, or headless labels; `agent_nickname` is not
  a grouping identity.
- Claude preserves known stable subagent types and collapses unknown temporary
  sidechain names to `Claude Subagent`.
- OMP recovers task agent roles from parent `task` calls and uses those roles as
  stable labels.
- Kimi uses explicit `config.update.profileName` values from the known profile
  set only; filesystem segments such as `main` and `agent-N` are not fallbacks.
- Messages without a recognized stable agent identity should not create an
  `Agents` row.

Changing parsed agent identity semantics must bump both source-message cache and
TUI cache schema versions so stale labels are rebuilt.

## Consequences

Client parsers may keep client-specific recovery logic, but the value written to
`UnifiedMessage.agent` must already be a stable reporting identity. Aggregation
and cache code should not attempt to reinterpret runtime labels after parsing.

New local client support must identify its stable agent field before populating
`UnifiedMessage.agent`. If no stable field exists, leave the agent unset instead
of deriving one from a display name or path.
