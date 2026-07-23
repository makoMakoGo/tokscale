# ADR 0007: Client identity catalog and local input authority

Status: Accepted

## Context

Tokscale needs a stable client identity for filters, reports, caches, and TUI
projections. It also needs a precise local-input contract for discovering and
parsing provider artifacts. Identity, acquisition, usage attribution, and
diagnostics are separate concerns:

- a **Client** identifies the application that produced a usage record;
- an **Input** is one filesystem or database unit acquired by an adapter;
- a **Provider** is optional model-usage attribution;
- **Data Health** describes input availability and record rejection; and
- a **Pricing Source** identifies pricing provenance.

Conflating these concepts makes presentation labels, filesystem paths, or model
providers behave like additional client identities.

## Decision

### Identity authority

`crates/tokscale-core/client-catalog.json` is the sole client identity catalog.
Each entry defines the Rust variant, public ID, display labels, and presentation
metadata. Generated `ClientId` data is the only client identity used by Rust
code.

The catalog IDs are the complete accepted namespace for:

- `--client`;
- `defaultClients`;
- report and cache payloads;
- TUI client selection; and
- keys in scanner settings that accept client IDs.

An unrecognized ID is an error. IDs are not inferred from paths, process names,
model names, providers, or display labels.

### Local-input authority

Every catalog entry has exactly one registered local-input adapter and one scan
definition. These three sets must have exact parity and no duplicate entries.

The scan definition and the adapter's discovery implementation are the sole
authority for fixed default roots beneath the selected home, filename
selection, companion files, database sidecars, and custom-root support.
Additional roots come only from `scanner.extraScanPaths`; OpenCode database
files come only from `scanner.opencodeDbPaths`. The adapter's session
schema/parser is the sole authority for accepted envelopes, database schemas,
required fields, record semantics, deduplication, and token interpretation.

`docs/clients.md` is the user-facing discovery map generated from that contract.
It does not create a second path or schema authority.

### Input semantics

Adapters acquire current provider-written local artifacts directly. Acquisition
does not invoke provider CLIs, inspect provider processes, call private remote
interfaces, or manufacture usage records.

An absent automatically discovered root means that the client has no input at
that location. Once a root or configured input exists, discovery, open, query,
snapshot, and parse failures remain visible through Data Health. Record-level
schema failures reject the affected records and preserve valid records from the
same input when the parser can continue. A failure for one client does not abort
unrelated clients.

Accepted usage records preserve their observed token values and canonical model
identity. Provider attribution may be inferred centrally from a valid model ID;
when it cannot be inferred, the provider is `unknown`. Provider attribution by
itself never determines record eligibility.

SQLite adapters that declare WAL-aware acquisition fingerprint and read the
database with its committed WAL state. Derived message shards and aggregate
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
3. one registered local-input adapter;
4. one current session schema/parser;
5. focused discovery, parser, and health tests;
6. catalog/scan/adapter parity checks; and
7. a current discovery row in `docs/clients.md`.

Changing a root, filename rule, companion dependency, database schema, or record
envelope requires an adapter/schema change with focused tests and a matching
documentation update. A parser behavior change that affects cached output also
requires a parser revision change.

## Consequences

Each accepted local integration has one public identity and one executable
input contract. Reports, filters, scanner configuration, caches, and TUI views
therefore share the same client namespace, while path and format evolution stays
owned by the adapter and schema that can validate it.
