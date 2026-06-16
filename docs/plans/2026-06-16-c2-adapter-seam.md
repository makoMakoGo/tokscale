# C2 — Local Source Adapter Seam (#36)

Status: implementation plan for the C2 tracer-client migration after C1 / PR #59.
C2 is producer-side only: it introduces a local source adapter seam and
migrates Zed, Pi, and OMP. Remaining clients stay on the legacy scanner/driver
path for C3.

## Scope

- Add `crates/tokscale-core/src/adapters/` with a `LocalSourceAdapter` seam.
- Move Zed discovery, SQLite/WAL fingerprinting, parsing, cache fold, and sink
  emission behind the Zed adapter.
- Move Pi discovery, file fingerprinting, parsing, cache fold, and sink emission
  behind the Pi adapter.
- Move OMP discovery, file fingerprinting, cache hit/miss split, parent-task
  agent index construction, parsing, cache fold, and sink emission behind the
  OMP adapter.
- Partition requested clients so migrated clients do not call the legacy
  scanner. An empty legacy partition must produce `ScanResult::default()`, not
  an accidental all-client scan.
- Include adapter-discovered source units in `compute_source_digest`, including
  SQLite WAL paths for Zed.

## Module Layout

```text
crates/tokscale-core/src/adapters/
  mod.rs
  cache.rs
  discover.rs
  zed.rs
  pi.rs
  omp.rs
```

Pi and OMP share the `sessions::pi` parser family, but they remain separate
adapter modules because they are separate client identities and OMP owns a
client-specific parent-task index policy.

## Guardrails

- Do not migrate remaining clients in C2.
- Do not add sharded cache, streaming fold, Codex/Claude adapters, fallback
  parser paths, mock success paths, or silent degradation.
- Keep generic cache semantics byte-compatible with the current helper logic:
  cache hits carry only a path through parse and are resolved by
  `SourceMessageCache::take_messages` during fold; fresh cache entries store raw
  parsed messages before pricing; current-run messages are priced after raw
  cache entry construction.
- Keep scanner discovery available for legacy clients and tests. C2 only stops
  migrated driver paths from reading tracer-specific `ScanResult` details.

## Validation

```bash
rtk cargo fmt --check
rtk cargo clippy -p tokscale-core --all-targets
rtk cargo test -p tokscale-core adapters:: -- --nocapture
rtk cargo test -p tokscale-core zed -- --nocapture
rtk cargo test -p tokscale-core pi -- --nocapture
rtk cargo test -p tokscale-core omp -- --nocapture
rtk cargo test -p tokscale-core --lib
rtk cargo test
```

Review grep before PR:

```bash
rtk rg "zed_db_paths|parse_zed_sqlite|parse_pi_file|parse_omp_file_with_parent_task_agent_index|build_omp_parent_task_agent_index" crates/tokscale-core/src/lib.rs
rtk rg "ClientId::Zed|ClientId::Pi|ClientId::Omp" crates/tokscale-core/src/lib.rs
```

Expected: production driver code has adapter partitioning/plumbing only; tracer
scan/parse/cache policy lives in `crates/tokscale-core/src/adapters/` or parser
tests.
