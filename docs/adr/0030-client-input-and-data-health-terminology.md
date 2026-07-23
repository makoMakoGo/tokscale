# ADR 0030: Separate client identity from scan inputs and data health

Status: Accepted

## Context

Tokscale historically used `source` for several unrelated concepts: the
product that produced usage, a filesystem or database artifact being scanned,
the health of that artifact, and pricing provenance. The TUI then exposed the
same identity as Source, Harness, and Client in different controls. This made
public contracts ambiguous and left internal names unable to distinguish a
client from an acquired input.

## Decision

Use one vocabulary throughout the maintained product surface:

- **Client** is the canonical product identity, such as Claude, Codex, or Zed.
- **Input** or **Scan Input** is one acquired filesystem or database unit.
- **Data Health** reports rejected records and unavailable or partial inputs.
- **Provider** is optional model-usage attribution.
- **Pricing Source** is the fully qualified name for pricing provenance.

This is a deliberate breaking migration. Public health JSON uses
`cleanInputs`, `degradedInputs`, `partialInputs`, `failedInputs`,
`inputDataBytes`, issue `client`, and `affectedInputs`. Input-level issue and
handling identifiers are `partial-input`, `input-unavailable`, and
`input-skipped`. The pricing selector is `--pricing-source`, and custom pricing
configuration and lookup DTOs use `pricingSource`. Old field names, issue
identifiers, and CLI flags have no aliases.

Internal acquisition, snapshot, cache, planning, parsing, and fold types use
`Input` terminology. TUI cache schema 45 is the only accepted generation
schema. Message-shard format 8 is the only format used by ordinary reads;
older recognized formats may be classified only by explicit pruning so they
can be deleted safely.

The following uses of `source` remain intentionally outside this migration:

- third-party wire or storage formats whose producer literally names a field
  or column `source`;
- Rust error chaining through `std::error::Error::source`, including the
  conventional cause field and local binding used to preserve that chain;
- ordinary references to program source code and the qualified domain term
  `Pricing Source`.

Maintained documentation and historical engineering records also use the
current vocabulary when they refer to these domain concepts, so repository
searches do not teach retired names to future changes.

## Consequences

Existing TUI generations and message shards rebuild once. Consumers of report
JSON, custom pricing configuration, and pricing lookup DTOs must adopt the new
field names immediately. The codebase gains one unambiguous identity axis and
one unambiguous acquisition axis instead of carrying compatibility branches
for a vocabulary that never described a stable domain boundary.
