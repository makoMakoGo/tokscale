# Tokenx maintainer context

Tokenx accounts for usage recorded by local AI coding clients. This file
summarizes the current product model; architecture decisions remain
authoritative when more detail is needed.

## Vocabulary

Terminology follows ADR 0007.

- `client` is the stable product identity of one concrete local tool.
  `ClientId` is generated from the client catalog and survives through
  acquisition, generation state, and projections.
- `integration driver` owns one client's source discovery, decoding, and
  record policy. The exhaustive integration registry binds an
  identity-neutral driver to a `ClientId`.
- `input` or `scan input` is one filesystem or database unit acquired by a
  client. Input diagnostics are exposed as Data Health.
- `model_id` is the canonical model identifier used for grouping and local
  pricing. Raw observed labels may contain route, tier, release-date, or
  free-channel decorations; acquisition normalizes them through the engine's
  model canonicalizer before aggregation and pricing. Date, release,
  free-channel, and route decorations are not preserved as model identity.
- `raw_model_label` is the non-empty model observation persisted by a client
  before final canonicalization. It remains valid usage identity when optional
  alias or provider enrichment is unavailable.
- `provider_id` is attribution metadata resolved from an explicit observed
  value, deterministic model-family inference, or `unknown`. It is not a
  prerequisite for retaining model and token facts.
- `workspace` is the local working-directory attribution exposed by
  projections.

## Decisions

- Do not add silent fallback, fake success, mock execution, or defensive
  degradation to make an unclear state look successful. Failures should surface
  as explicit errors, logs, or failing tests.
- Do not reject a positive, timestamped usage record with a non-empty model
  label merely because provider attribution cannot be resolved. Keep the model
  and tokens, infer centrally when possible, and otherwise use `unknown`.
- Read local client storage through the registered integration for its
  accepted current format, as established by ADR 0007. Schema and database
  I/O/query failures are explicit errors.
- Keep Claude Code handling for `model = "<synthetic>"` placeholder records.
  That placeholder is malformed input cleanup, not a real model or client.
- Keep Pi and OMP as separate client identities. OMP usage must not be
  counted as Pi usage by display or aggregation code.
- Treat `cwd` workspace attribution as an engine rule, not as caller
  folklore. Every projection uses the same workspace rules.

## Architecture

- `tokenx-engine` owns acquisition and produces one immutable `Generation`
  containing the canonical usage index, sessions, input footprint, health,
  and diagnostics.
- The exhaustive registry is the only binding from catalog identity to an
  integration driver. Decoders emit source-neutral records; the runner applies
  the bound `ClientId`.
- `tokenx` owns CLI/TUI lifecycle and presentation. Models and every TUI
  screen are projections of an installed generation; projection controls do
  not scan or write caches.
- The task supervisor owns background acquisition and subscription work and
  drains it before terminal shutdown.
- Generation and input-record caches are disposable accelerators. They never
  own product facts or aggregation rules.

## Non-goals

- Tokenx does not provide migration APIs, compatibility namespaces, generic
  report frameworks, hosted services, or invented success states.
