# tokscale personal/local-clients context

This fork is maintained for local client usage accounting on the
`personal/local-clients` branch. Upstream changes are reviewed and ported
selectively; upstream content is not merged wholesale. Branch decisions in this
file take precedence when upstream semantics conflict with local needs.

## Vocabulary

- `source` is the client or data origin that produced usage records.
- `client` is a concrete local tool or integration with its own files, parsing
  policy, display facts, and filters.
- `model_id` is the raw model identifier used for pricing and grouping. Keep it
  raw unless a documented pricing helper explicitly normalizes it.
- `workspace` is the local working directory attribution used by reports and
  the TUI.

## Decisions

- Do not add silent fallback, fake success, mock execution, or defensive
  degradation to make an unclear state look successful. Failures should surface
  as explicit errors, logs, or failing tests.
- Keep Claude Code handling for `model = "<synthetic>"` placeholder records.
  That placeholder is malformed input cleanup, not a real source or client.
- Remove upstream `synthetic.new` as a source/client concept. It does not belong
  in filters, scanner defaults, TUI source pickers, frontend logos, or docs.
- Keep Pi and OMP as separate client/source identities. OMP usage must not be
  counted as Pi usage by display or aggregation code.
- Treat `cwd` workspace attribution as branch behavior, not as caller folklore.
  Reports and TUI views should share the same workspace rules.

## Architecture Direction

- Client identity should come from a small catalog of display facts and stable
  ids, not from repeated switch statements across core, CLI, TUI, and frontend.
- Local parsing policy should move behind client adapters one client at a time.
  Do not design a large framework before a tracer-bullet migration proves the
  interface.
- Usage aggregation should become a deep core module shared by report and TUI
  paths. Cache may store inputs or outputs, but must not become a second source
  of aggregation rules.
- TUI views should share an interaction seam for scroll, hitbox, and selection
  behavior where duplication is already causing drift.

## Non-goals

- This branch does not attempt to mirror every upstream client idea.
- This branch does not preserve compatibility shims for concepts that have been
  rejected locally.
- This branch does not hide parser, scanner, pricing, or aggregation errors in
  order to keep the UI quiet.
