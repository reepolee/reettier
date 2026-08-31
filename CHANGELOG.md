# Changelog









## [26.8.7] - 2026-08-31

## [26.8.6] - 2026-08-24

### Fixed
- `{#switch}`/`{#case}`/`{:else}` inside `<script>` blocks are no longer
  fed to the JS formatter as code (which added stray semicolons and
  flattened nesting). They now indent like markup: the switch at the
  script's base level, cases one level deeper, bodies two deeper, and
  `{/switch}` back at the base.


## [26.8.5] - 2026-08-24

### Added
- Indenter and reprinter (`--full`) now format `{#switch}`/`{#case}` blocks:
  cases indent one level under the switch, bodies one level under their case,
  and `{/switch}` returns to the switch's column. `{:else}` arms indent like
  cases; the reprinter parses arms into its AST instead of mangling them.
- Reprinter (`--full`) runs complex `{#case}` and `{#switch}` expressions
  through the JS formatter (operators, calls, ternaries), leaving simple
  literals and identifier paths untouched so quoting is never rewritten.


## [26.8.4] - 2026-08-19

## [26.8.3] - 2026-08-11

## [26.8.2] - 2026-08-10

## [26.8.1] - 2026-08-10

## [26.8.0] - 2026-08-10

## [26.7.7] - 2026-07-27

## [26.7.6] - 2026-07-18

## [26.7.5] - 2026-07-18

### Added
- Indenter now normalizes redundant statement-level semicolons: empty-statement
  lines (a lone `;`) and `;;` runs collapse to a single terminator. Semicolons
  inside `for (…;…;…)` headers and real empty loop bodies (`while (x);`) are left
  untouched. The self-verify net recognizes the removal, so files are never reverted for it.


## [0.1.0] - 2026-07-01

- Initial release. A **layout-preserving** formatter for `.ree` templates and
  their embedded JS/TS/CSS — the author's line breaks are the source of truth;
  reettier only normalizes indentation, spacing, and group shape.
- JS/TS: bracket-depth indentation, first-boundary group explode/collapse switch
  (with emergent hugging), managed trailing commas, Tier-1 punctuation spacing.
- CSS: strict bracket-only groups; rule blocks and selector/value lists preserved.
- `.ree`: markup indentation by HTML-tag and Ree-directive nesting (incl. multi-line
  attribute tags), with embedded `{{ }}` JS, `<script>` JS, and `<style>` CSS
  reformatted in place.
- Self-verifying: if formatting wouldn't preserve the token stream, the original
  file is emitted unchanged — corruption is impossible by construction.
