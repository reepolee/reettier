# Merge reefmt into reettier as a `--full` mode

## Status

accepted

## Context

We shipped two formatter binaries: `reefmt` (an AST reprinter that re-derives
all line breaks from the syntax tree) and `reettier` (a layout-preserving
indenter where the author's own line breaks are the source of truth). Daily work
wants the indenter - developers deliberately break or collapse code to steer
readability, and a reprinter erases that intent. But the reprint behavior is
still occasionally wanted for a full re-layout to house style. Maintaining two
binaries, two config schemas, and two CLIs is the cost we no longer want.

## Decision

Merge the two into a single binary named **reettier**. The default (no mode
flag) is the Indenter. `--full` selects the Reprinter (the former reefmt
engine), universally - including `--stdin`. The `reefmt` repo/binary is erased.

- Both engines are vendored into `reettier/`; `--full` output stays
  byte-identical to today's reefmt (that is the point of keeping it - see
  [[0002-self-verify-both-modes]] for the safety story).
- One optional `reettier.jsonc`. Top-level keys drive file discovery (shared by
  both modes) and the Indenter. Reprinter width/collapse knobs live under a
  nested `"full"` block. A missing config is never an error (reefmt used to
  require its config - that regression is not carried over).
- Discovery is mode-agnostic: `reettier` and `reettier --full` format the exact
  same file set; they differ only in layout.
- The CLI stays lean. Reprinter knobs are **config-only**, not CLI flags, so
  `--full` has one reproducible behavior per repo.
- `removeUnusedImports` (reefmt's only code-deleting behavior) is dropped - it
  was never used, and no formatting mode should edit semantics.

## Consequences

- The binary carries reefmt's heavy `swc_core` + `malva` dependencies even
  though most invocations only indent. Accepted: it keeps "`--full` = reefmt
  style" literally true, and reettier's "zero *runtime* deps / no Node" claim
  still holds (SWC is Rust).
- Existing `reefmt.jsonc` files (in reepolee, reeweb, and this repo) must be
  migrated to `reettier.jsonc` with reprint knobs moved under `"full"`.
- Scripts that passed reprint flags (e.g. `reefmt --wrap-width 120`) must move
  those settings into config. Acceptable pre-1.0.
