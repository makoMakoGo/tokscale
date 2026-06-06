# ADR 0004: cwd workspace attribution is branch behavior

Status: Accepted

## Context

Local reports and TUI views need consistent workspace attribution. If cwd
behavior lives as caller knowledge, CLI output, TUI output, and cache behavior
can drift.

## Decision

Treat cwd workspace attribution as a documented branch behavior. Report and TUI
paths should share the same workspace rules through core code rather than
reconstructing them independently.

## Consequences

Future aggregation and cache changes should verify workspace attribution with
focused tests. Unknown or ambiguous workspace state should surface clearly
instead of being silently mapped to a convenient bucket.
