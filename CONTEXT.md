# reettier - Context

Glossary for the `reettier` formatter. Definitions only; no implementation
detail. See `docs/adr/` for decisions with trade-offs.

## Terms

### Indenter (default mode)

The layout-preserving formatter. The author's own line breaks are the source of
truth; the indenter only fixes indentation, punctuation spacing, and group
shape. It never introduces or removes line breaks on its own. This is what
`reettier` does with no mode flag. See README "The four rules".

### Reprinter (`--full` mode)

The AST-reprinting formatter, run with `--full`. It discards the author's line
breaks and re-derives layout from the syntax tree, wrapping and collapsing to
width/count limits. This is the behavior formerly shipped as the separate
`reefmt` binary, absorbed into reettier during the merge (see
[[adr-0001-merge-reefmt-into-reettier]]). A developer reaches for it when they
want a full re-layout rather than tidying of their own layout.

### Full re-layout

Shorthand for what the Reprinter does: throwing away existing line breaks and
regenerating them. Contrast with the Indenter, which preserves them. "Run the
full reprinter" = "do a full re-layout".

### reettier.jsonc

The single, optional config file. A missing file means sane defaults (never an
error). Top-level keys drive **file discovery** (shared by both modes) and the
Indenter (notably `indent`). Reprinter-only knobs (width/collapse limits) live
under a nested `"full"` block, so default-mode users never see them. There is
no separate `reefmt.jsonc` after the merge.

Discovery is **mode-agnostic**: `reettier` and `reettier --full` format the
exact same file set (same `skipDirs`/`skipFiles`/`skipExtensions`/`extensions`/
`skipDotDirs`); they differ only in how each file's contents are laid out. The
`"full"` block never carries its own discovery keys.

The Reprinter **only re-lays-out; it never deletes code.** reefmt's
`removeUnusedImports` is dropped in the merge (never used) - no formatting mode
edits semantics.

### `--full` is universal

`--full` selects the Reprinter for **every** entry path, including `--stdin`:
`reettier --full --stdin .ts` reprints, bare `reettier --stdin .ts` indents.
One mental model - `--full` = reprint, everywhere. `--stdin` with no extension
defaults to `.ree` (reettier is `.ree`-first; this is unchanged from reefmt's
`.ts` default).

### Self-verify (never corrupts)

A binary-wide invariant: **no formatting mode may change meaning-bearing tokens
or drop comments.** If a format would, the file is left unchanged and the
failure is reported - output is never written corrupt. Each engine keeps its
own checker: the Indenter verifies its significant-token stream; the Reprinter
re-parses its output and compares semantic AST tokens + comments (the check
ignores whitespace/line breaks, so collapsing and wrapping are fine - only real
token loss or a comment drop trips it). This is why a "layout" tool carries a
full AST walker; see [[adr-0002-self-verify-both-modes]].

## Conventions

- **Default mode is the Indenter.** Every repo in `labs/` formats with bare
  `reettier` (Indenter) in its hooks and CI. `--full` is opt-in per invocation.
- **Reformat before public shipping.** Code is re-run through the formatter
  before any public release, so day-to-day layout can stay author-steered.
- **Lean CLI; Reprinter knobs live in config only.** Width/collapse limits are
  not CLI flags. They live solely in the `reettier.jsonc` `"full"` block, so
  `--full` has one reproducible behavior per repo. The CLI is `--full`,
  `--check`/`-c`/`--dry-run`, `--diff`, `--git`, `--verbose`, `--stdin`,
  `--init`, `--version`, `--help` (plus hidden `--depths`/`--dump-sig`).
