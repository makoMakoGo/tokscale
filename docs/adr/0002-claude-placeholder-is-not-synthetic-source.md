# ADR 0002: Claude placeholder is not a Synthetic source

Status: Accepted

Related issue: #34

## Context

Claude Code can emit usage records where the model field is the placeholder
`"<synthetic>"`. That value appears in malformed or non-billable records and
must not pollute usage totals.

Upstream also introduced a separate `synthetic.new` source/client concept. The
name overlap makes the code look as if both behaviors belong to the same
domain, but they do not.

## Decision

Keep the Claude Code placeholder drop logic for `model = "<synthetic>"`.

Remove upstream `synthetic.new` as a source/client concept. It should not appear
in source filters, default source lists, scanner source injection, TUI source
pickers, frontend source logos, README platform lists, or session readers.

## Consequences

Any useful pricing normalization currently attached to Synthetic code must move
to a pricing helper with explicit tests. It must not keep using Synthetic source
or client vocabulary.
