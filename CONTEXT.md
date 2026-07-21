# tokscale personal/local-clients context

This fork is maintained for local client usage accounting on the
`personal/local-clients` branch. Upstream changes are reviewed and ported
selectively; upstream content is not merged wholesale. Branch decisions in this
file take precedence when upstream semantics conflict with local needs.

## Vocabulary

Terminology follows ADR 0030.

- `client` is the canonical product identity for a concrete local tool or
  integration, including its parsing policy, display facts, and filters.
- `input` or `scan input` is one filesystem or database unit acquired by a
  client. Input diagnostics are exposed as Data Health.
- `model_id` is the canonical model identifier used for grouping and local
  pricing. Raw observed labels may contain route, tier, release-date, or
  free-channel decorations; local report finalization normalizes them through
  the core model canonicalizer before aggregation and pricing. Date, release,
  free-channel, and route decorations are not preserved as model identity in
  this branch.
- `raw_model_label` is the non-empty model observation persisted by a client
  before final canonicalization. It remains valid usage identity when optional
  alias or provider enrichment is unavailable.
- `provider_id` is attribution metadata resolved from an explicit observed
  value, deterministic model-family inference, or `unknown`. It is not a
  prerequisite for retaining model and token facts.
- `workspace` is the local working directory attribution used by reports and
  the TUI.

## Decisions

- Do not add silent fallback, fake success, mock execution, or defensive
  degradation to make an unclear state look successful. Failures should surface
  as explicit errors, logs, or failing tests.
- Do not reject a positive, timestamped usage record with a non-empty model
  label merely because provider attribution cannot be resolved. Keep the model
  and tokens, infer centrally when possible, and otherwise use `unknown`.
- Read local client storage in its accepted current format only, as established
  by ADR 0019. OpenCode reads current SQLite databases, not legacy message JSON;
  obsolete schemas and database I/O/query failures are explicit errors.
- Keep Claude Code handling for `model = "<synthetic>"` placeholder records.
  That placeholder is malformed input cleanup, not a real model or client.
- Remove upstream `synthetic.new` as a client concept. It does not belong in
  filters, scanner defaults, TUI Client pickers, or docs.
- Keep Pi and OMP as separate client identities. OMP usage must not be
  counted as Pi usage by display or aggregation code.
- Treat `cwd` workspace attribution as branch behavior, not as caller folklore.
  Reports and TUI views should share the same workspace rules.

## Architecture Direction

- Client identity should come from a small catalog of display facts and stable
  ids, not from repeated switch statements across core, CLI, and TUI.
- This fork's active product surface is local Rust CLI/TUI. Hosted account
  auth, hosted data submission, and the Next.js social frontend were removed
  by ADR 0015.
- Local parsing policy should move behind client adapters one client at a time.
  Do not design a large framework before a tracer-bullet migration proves the
  interface.
- Usage aggregation should become a deep core module shared by report and TUI
  paths. Caches may store derived data but never own aggregation rules.
- TUI views should share an interaction seam for scroll, hitbox, and selection
  behavior where duplication is already causing drift.

## Non-goals

- This branch does not attempt to mirror every upstream client idea.
- This branch does not preserve compatibility shims for concepts that have been
  rejected locally.
- This branch does not hide parser, scanner, pricing, or aggregation errors in
  order to keep the UI quiet.
