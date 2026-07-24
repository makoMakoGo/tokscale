# ADR 0029: Semantic TUI theme contract

Status: Accepted

## Context

The TUI historically treated `green`, `halloween`, `teal`, `blue`, `pink`,
`purple`, `orange`, `monochrome`, and `ylgnbu` primarily as five-step
contribution palettes. Later `graphite`, `lagoon`, and `dusk` also supplied
surface, text, chrome, and table colors. All twelve values were exposed through
the same `ThemeName`, `--theme` option, and persisted `colorPalette` setting.

This mixed contract made most named themes visually identical outside Stats.
Renderers also owned many raw terminal colors for metrics, status, actions, and
decorative surfaces, so theme scope was implicit and inconsistent.

## Decision

Every `ThemeName` is a complete TUI theme. A theme supplies colors by semantic
role rather than exposing a single accent plus unrelated raw colors:

- surfaces: canvas, panel, alternate row, and current row;
- text: primary, muted, disabled, and inverse;
- chrome: active navigation, headings, borders, focus, and current-period
  emphasis;
- selection: foreground and background;
- metrics: token, cost, input, output, cache, rate, and total values;
- status: success, warning, danger, information, and pending states;
- visualization: the activity ramp, chart grid, highlight, and artwork.

Renderers request these roles and do not decide literal colors. A role may use
the same value as another role within a theme, but its meaning remains explicit.
Theme changes may therefore affect every TUI surface while preserving layout,
data, and interaction behavior.

Model-family and client-catalog colors remain identity colors rather than theme
decoration. They retain their authoritative source and are adapted only for
contrast against non-selected panel and table surfaces. Selection state takes
precedence over identity: selected identity text uses the semantic selection
foreground instead of forcing a brand color onto an unrelated background.

Status roles retain stable meaning across themes. A theme may choose its own
shade, but success, warning, and danger must remain distinguishable.

All twelve themes are RGB themes. The TUI does not inspect terminal color
capability and does not maintain an ANSI downgrade palette. `NO_COLOR`,
`TERM=dumb`, and terminal-brand detection do not alter theme construction. If a
genuine no-color product requirement appears later, it must be designed as an
explicit rendering mode rather than folded into theme identity.

The persisted `colorPalette` key and all twelve existing values remain readable
and writable in this change. Renaming the key is a separate configuration
migration and is not required to establish the semantic rendering contract.

## Consequences

- Switching any named theme visibly changes ordinary pages, navigation, and
  selection state instead of only the Stats activity graph.
- Exact color assertions target semantic roles, while theme-matrix tests cover
  contrast, activity ordering, and distinct theme signatures.
- Adding a renderer color requires selecting or introducing a semantic role.
- Existing saved theme names retain their syntax but intentionally gain broader
  visual effect.
- Theme definitions become larger because each one owns a complete, testable
  palette.

## Rejected alternatives

- Keeping nine activity-only palettes and three full themes preserves the
  ambiguous user contract.
- Replacing every literal color with one generic accent destroys metric,
  identity, and status semantics.
- Splitting surface theme and activity palette into independent settings adds a
  product choice that is not currently required. It can be reconsidered if
  users need arbitrary combinations.
- Automatically mapping RGB themes to a shared ANSI palette adds a second color
  system, collapses distinct theme names to one presentation, and has no stated
  product requirement.
