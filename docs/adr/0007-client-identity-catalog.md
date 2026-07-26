# ADR 0007: Client identity catalog and local input authority

Status: Accepted; integration ownership revised by ADR 0030

## Context

Tokenx needs a stable client identity for filters, reports, caches, and TUI
projections. It also needs a precise local-input contract for discovering and
parsing provider artifacts. Identity, acquisition, usage attribution, and
diagnostics are separate concerns:

- a **Client** identifies the application that produced a usage record;
- an **Input** is one filesystem or database unit acquired by an integration;
- a **Provider** is optional model-usage attribution;
- **Data Health** describes input availability and record rejection; and
- a **Pricing Source** identifies pricing provenance.

Conflating these concepts makes presentation labels, filesystem paths, or model
providers behave like additional client identities.

## Decision

### Identity authority

`crates/tokenx-engine/client-catalog.json` is the sole client identity catalog.
Each entry defines the Rust variant, public ID, display labels, and presentation
metadata. Generated `ClientId` data is the only client identity used by Rust
code. Reports and TUI controls render client labels directly from this catalog;
local configuration cannot override them.

The catalog IDs are the complete accepted namespace for:

- `--client`;
- `defaultClients`;
- report and cache payloads;
- TUI client selection; and
- keys in scanner settings that accept client IDs.

An unrecognized ID is an error. IDs are not inferred from paths, process names,
model names, providers, or display labels.

### Local-input authority

Every catalog entry is exhaustively dispatched to exactly one
`IntegrationDriver`. The dispatch is a wildcard-free `match` on `ClientId`, so
adding a catalog variant without selecting an integration is a compile error.

The exhaustive registry owns each `ClientId` binding. Its selected driver owns
the source definition, discovery, decoder selection, and fold without declaring a
second identity. The binding is the sole authority associating discovered
inputs and emitted usage with a client. Discovery remains authoritative for
fixed default roots beneath the selected home, filename selection, companion
files, database sidecars, and custom-root support. Additional roots come only from
`scanner.extraScanPaths`; OpenCode database files come only from
`scanner.opencodeDbPaths`.

The integration's decoder and schema are the sole authority for accepted
envelopes, database schemas, required fields, record semantics, deduplication,
and token interpretation, but not source identity. Each source-neutral
`DiscoveredInput` contains one `DecoderKind` variant that carries the semantic
revision and any execution detail required by that decoder. Its persisted
`DecoderVersion` is derived from the same value, so cache identity cannot drift
from decoder selection. There is no client-to-decoder default mapping.

Decoders produce one source-neutral `UsageRecord` representation. Derived
input-record shards store that same type. The runner constructs a
`BoundUsageSink` from the selected integration binding; the sink accepts only
`UsageRecord` and attaches the integration's typed `ClientId` to construct a
`AttributedUsageRecord`. `AttributedUsageRecord` composes client attribution with a
`UsageRecord` instead of duplicating the usage fields. Neither an input,
decoder, integration fold, nor cache shard can supply a competing client identity,
so mismatched source attribution is not representable.

`docs/clients.md` is the user-facing discovery map generated from that contract.
It does not create a second path or schema authority.

### Input semantics

Integrations acquire current provider-written local artifacts directly. Acquisition
does not invoke provider CLIs, inspect provider processes, call private remote
interfaces, or manufacture usage records.

An absent automatically discovered root means that the client has no input at
that location. Once a root or configured input exists, discovery, open, query,
snapshot, and parse failures remain visible through Data Health. Record-level
schema failures reject the affected records and preserve valid records from the
same input when the decoder can continue. A failure for one client does not abort
unrelated clients.

Accepted usage records preserve their observed token values and canonical model
identity. Provider attribution may be inferred centrally from a valid model ID;
when it cannot be inferred, the provider is `unknown`. Provider attribution by
itself never determines record eligibility.

SQLite integrations that declare WAL-aware acquisition fingerprint and read the
database with its committed WAL state. Derived input-record shards and aggregate
caches are reproducible acceleration artifacts, not local usage authorities.

### Public diagnostics

`docs/clients.md` is the complete public discovery map. Executing `models`
reports Data Health in its JSON envelope and on stderr; the TUI exposes the
same input availability and rejection domain in Data Health. There is no
separate command that rediscovers paths or parses another copy of client rules.

Reports and the TUI use **Data Health** for unavailable, partial, and
record-rejection diagnostics. The Overview health fact is **Inputs Healthy**.

### Extension rules

Adding a client requires one atomic contract change containing:

1. one catalog identity;
2. one scan definition;
3. one vertical `IntegrationDriver` selected by the exhaustive dispatch;
4. one current source-neutral decoder kind and session schema;
5. focused discovery, decoder, and health tests;
6. a catalog/integration identity check; and
7. a current discovery row in `docs/clients.md`.

Changing a root, filename rule, companion dependency, database schema, or record
envelope requires an integration/schema change with focused tests and a matching
documentation update. A decoder behavior change that affects cached output also
requires a decoder revision change.

## Consequences

Each accepted local integration has one public identity and one executable
input contract. Projections, filters, scanner configuration, caches, and TUI views
therefore share the same client namespace. The production pipeline gives only
the selected integration authority to attach `ClientId`; the source-neutral input,
decoder output, persisted shard, and diagnostic errors carry no competing
client field. Runtime decoder selection is one `DecoderKind`, and persisted
cache identity is derived from it, so execution behavior and cache identity
cannot drift independently; path and format evolution stays owned by the
integration and schema that can validate it.
