# PLAN: Merge reefmt into reettier as `--full` mode

Design settled in the grill session. See CONTEXT.md + docs/adr/0001, 0002.

## Goal

One binary `reettier`. Default = Indenter (existing reettier engine). `--full` =
Reprinter (vendored reefmt SWC/malva engine). Single optional `reettier.jsonc`
with reprint knobs under a nested `"full"` block. `reefmt/` erased.

## Rules honored

- Zero runtime deps beyond what reefmt already used (swc_core, malva). snake_case
  in Rust. Minimal changes. No comment removal. No em-dashes/box-drawing.

## Steps

- [ ] 1. Vendor reefmt engine modules into `reettier/src/full/` (reprinter path):
      ree_format.rs, format.rs, ree_parser.rs, swc_format.rs, swc_printer/**,
      ast_check.rs. Drop remove_unused_imports.rs. Fix module paths + drop the
      `remove_unused` param threading.
- [ ] 2. Cargo.toml: add swc_core + malva deps (from reefmt). Keep release profile.
- [ ] 3. config.rs: add nested `full` block (FullConfig) with reprint knobs;
      keep it optional with sane defaults. Map to reefmt's CollapseConfig.
- [ ] 4. format.rs (dispatch): add `full: bool` param. When true, route
      ree/ts/js/css to the reprinter; else existing indenter.
- [ ] 5. main.rs: parse `--full`; thread through file mode + `--stdin`. Add
      `--init` scaffolding a fresh reettier.jsonc. Update help text.
- [ ] 6. Fix stale docstrings in main.rs/config.rs pointing at "reefmt repo".
- [ ] 7. `cargo build --release` + `cargo test` green.
- [ ] 8. Smoke test: indenter default vs `--full` on a sample; `--stdin` both ways.
- [ ] 9. Erase `reefmt/` folder.

## Progress log

- Step 1 DONE: vendored reefmt modules into src/full/, rewrote crate:: paths to
  crate::full::, dropped format_file (reefmt-main IO coupling), added
  src/full/mod.rs with format_full() entry. remove_unused hardwired false
  (config surface dropped) instead of a risky rip-through of swc_printer.
- Step 2 DONE: Cargo.toml swc_core + malva added. Release build green.
- Step 3 DONE: FullConfig nested block + collapse_config() mapping.
- Step 4 DONE: format_source_with(full) dispatch; unknown-ext guard shared.
- Step 5 DONE: main.rs parses --full, --init; threads full through file loop +
  --stdin; help updated. reettier.jsonc.template added.
- Step 6 DONE: fixed stale reefmt-repo docstring in main.rs header.
- Self-verify: moved reefmt's ast_check into full::format_full so BOTH files and
  --stdin get it (reefmt only checked inside format_code_file, which we don't
  use). On failure, original returned unchanged. Satisfies ADR-0002.
- Step 7 DONE: cargo test = 297 pass, 5 fail. All 5 failures are PRE-EXISTING
  (verified: 3 engine:: tests fail on clean reettier HEAD; 2 ree_parser:: tests
  fail identically in the original reefmt repo). None caused by the merge.
- Step 8 DONE: smoke tests confirm indenter vs --full diverge as designed on
  .ts (object collapse + `;` + `x+y`->`x + y` only under --full). .ree both
  modes OK. --init scaffolds + refuses overwrite; scaffolded config parses and
  drives both modes. Self-verify wired (ast_check unit tests green).
- Step 9 DONE: reefmt/ was clean + fully pushed to origin (0 ahead/behind);
  erased locally. reettier builds standalone afterward.

## RESULT: merge complete. reettier is the single binary; --full = reprinter.

## Bug fixes (follow-up): all 5 pre-existing failures fixed -> 302 pass, 0 fail.
- engine (3 tests): removed the `is_paren_group` exclusion in the synthetic
  trailing-comma logic (engine.rs). Exploded function-call `foo(...)` groups now
  get a trailing comma (valid JS since ES2017); rest/spread last elem still
  excluded via last_elem_is_rest, CSS + semicolon-groups still excluded.
- ree_parser (2 tests):
  1. Glued brace after tag name (`<details{~ ... }>`): added open_tag_head()
     helper - no space before a first attr that begins with `{`; applied at all
     5 open-tag print sites.
  2. Bare unclosed opening tag (`<nav ...{~ ... }>`): added Element.unclosed
     field; when parse finds no matching close and no children, preserve the
     opening tag verbatim instead of synthesizing `</tag>`.
- Verified idempotent + end-to-end on the release binary.
