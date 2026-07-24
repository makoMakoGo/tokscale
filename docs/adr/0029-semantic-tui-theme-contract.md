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
- text: primary and secondary;
- chrome: active navigation, headings, borders, focus, and current-period
  emphasis;
- selection: foreground and background;
- metrics: tokens, cost, input, output, cache read, cache write, rate, total,
  and secondary cost;
- status: success, warning, danger, information, and pending states;
- visualization: contribution empty, four active contribution grades, track,
  chart grid, chart highlight, and artwork.

Renderers request these roles and do not decide literal colors. A role may use
the same value as another role within a theme, but its meaning remains explicit.
Theme changes may therefore affect every TUI surface while preserving layout,
data, and interaction behavior.

Text roles are deliberately small. `primary` carries normal content and
`secondary` carries supporting content while remaining readable. There is no
general-purpose `muted` or `disabled` gray that exempts visible labels from the
text contrast contract. Interaction availability is represented by interaction
state; explanatory text still uses one of the readable text roles.

Role-to-surface use is explicit:

| Surface | Permitted readable roles |
| --- | --- |
| canvas | text primary and secondary |
| panel | text primary and secondary; active navigation, heading, focus, current chrome, metric, and status roles |
| alternate/current row | text primary and secondary; current chrome, metric, and status roles used by that row |
| selection background | selection foreground; text, current chrome, metric, and status roles explicitly retained by selected table cells |

Identity colors are permitted on panel, alternate-row, and current-row
surfaces. Contribution colors are permitted on the graph panel. Decorative
track, grid, and artwork roles are not substitutes for readable text. This
matrix defines the combinations that theme tests must enforce; arbitrary
cross-products between unrelated roles and surfaces are not part of the
contract. Borders are non-text chrome and are therefore outside the 4.5:1 text
matrix.

Readable text must reach a 4.5:1 contrast ratio against every surface on which
its role is permitted. Meaningful non-text graph marks must reach 3:1 against
their panel surface. The contribution graph does not rely on color alone:
`Empty`, `Low`, `Medium`, `High`, and `Peak` use distinct glyph density as well
as the theme's empty marker and four-color active ramp. A missing calendar cell
remains structurally absent rather than masquerading as an empty day. Adjacent
activity grades therefore remain distinguishable without demanding an
impossible 3:1 ratio between every step of a compact five-level ramp.

Model-family and client-catalog colors remain identity colors rather than theme
decoration. They retain their authoritative source and are adapted only for
contrast against non-selected panel and table surfaces. Selection state takes
precedence over identity: selected identity text uses the semantic selection
foreground instead of forcing a brand color onto an unrelated background.
The adapted identity palette is resolved once when a theme is constructed;
renderers perform a direct lookup and never recompute WCAG blending per frame.

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
  the permitted role-to-surface contrast matrix, activity ordering, redundant
  contribution glyph encoding, and distinct theme signatures.
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
