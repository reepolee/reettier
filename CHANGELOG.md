# Changelog




































## [Unreleased]

### Added
- Indenter now normalizes redundant statement-level semicolons: empty-statement
  lines (a lone `;`) and `;;` runs collapse to a single terminator. Semicolons
  inside `for (…;…;…)` headers and real empty loop bodies (`while (x);`) are left
  untouched. The self-verify net recognizes the removal, so files are never
  reverted for it.

## [26.7.4] - 2026-07-11

## [26.7.3] - 2026-07-11

## [26.7.2] - 2026-07-11

## [26.7.1] - 2026-07-10

## [0.1.31] - 2026-07-10

## [0.1.30] - 2026-07-10

## [0.1.29] - 2026-07-09

## [0.1.28] - 2026-07-09

## [0.1.27] - 2026-07-09

## [0.1.26] - 2026-07-08

## [0.1.25] - 2026-07-08

## [0.1.24] - 2026-07-08

## [0.1.23] - 2026-07-08

## [0.1.22] - 2026-07-08

## [0.1.21] - 2026-07-05

## [0.1.20] - 2026-07-05

## [0.1.19] - 2026-07-04

## [0.1.18] - 2026-07-04

## [0.1.17] - 2026-07-03

## [0.1.16] - 2026-07-03

## [0.1.15] - 2026-07-03

## [0.1.14] - 2026-07-02

## [0.1.13] - 2026-07-02

## [0.1.12] - 2026-07-02

## [0.1.11] - 2026-07-02

## [0.1.10] - 2026-07-02

## [0.1.9] - 2026-07-01

## [0.1.8] - 2026-07-01

## [0.1.7] - 2026-07-01

## [0.1.6] - 2026-07-01

## [0.1.5] - 2026-07-01

## [0.1.4] - 2026-07-01

## [0.1.3] - 2026-07-01

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
