# ADR 0009: Ahead-only upstream policy and the unified architecture track

Status: Accepted

## Context

`personal/local-clients` has tracked `junhoyeo/tokscale` upstream since the
fork, merging main regularly (ADR 0001-0008 all assume that posture). Two
things changed:

1. ADR 0008's Phase A+B landed: `UnifiedMessage` now uses interned
   `Arc<str>` identity fields, a hashed `dedup_key`, and a derived date.
   Upstream parser changes already need mechanical adaptation on merge.
2. The remaining roadmap (streaming fold aggregation, sharded message
   cache, client adapters, a unified aggregation module) rewrites the
   parse driver and aggregation surfaces — the exact zones where every
   upstream client/feature commit lands. After that work, structural
   merge conflicts are permanent, not occasional.

Holding the architecture hostage to merge-ability would mean forgoing the
deep-module cleanup this fork exists for (#33).

## Decision

- The branch is **ahead-only** from 2026-06-13. We do not merge
  `origin/main` wholesale anymore.
- Upstream changes are **ported, not merged**: cherry-pick or hand-port
  leaf-level fixes we want (pricing data and lookup fixes, parser format
  fixes for clients we use, security fixes). Port commits reference the
  upstream SHA in the body: `ported from upstream <sha>`.
- New upstream clients are adopted by writing an adapter in our
  architecture, treating the upstream parser as a reference
  implementation, not a patch source.
- The unified architecture track (streaming fold + sharded cache + client
  adapters + deep aggregation module — issues #54, #36, #37 under #33) is
  one campaign with output-parity gates between phases, not three
  separate refactors.
- Frontend/platform directories (`packages/`) stay close to upstream and
  may still take direct ports; they are outside the core rewrite.

## Consequences

- Core refactors no longer pay an upstream-conflict tax; interfaces can
  be shaped for clarity instead of merge-ability.
- Upstream fixes arrive only when we notice and port them. Periodically
  review `git log origin/main` (e.g. when something breaks or monthly)
  for portable fixes.
- `git merge origin/main` on this branch is an error from now on; if one
  is attempted it will conflict structurally and should be aborted.
