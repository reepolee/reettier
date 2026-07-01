# Changelog



## [0.1.2] - 2026-07-01

## [0.1.1] - 2026-07-01

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
