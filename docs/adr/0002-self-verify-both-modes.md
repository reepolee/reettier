# Self-verify applies to both modes; each keeps its own checker

## Status

accepted

## Context

reettier's headline promise is "it can never corrupt your code": the Indenter
verifies it preserved the significant-token stream before writing, else leaves
the file unchanged. The merged-in Reprinter ([[0001-merge-reefmt-into-reettier]])
has a *different* checker inherited from reefmt (`ast_check.rs`): it re-parses
its own output and compares the semantic AST token sequence plus the set of
comments. A reprinter that reorders or drops tokens is the scarier failure
mode, so the guarantee matters most exactly where the layout is thrown away.

## Decision

Make "never corrupts" a **binary-wide invariant** covering both modes: if a
format would change any meaning-bearing token or drop a comment, the file is
left unchanged and the failure is reported. Keep each engine's own checker
rather than unifying them - the Indenter's significant-token check and the
Reprinter's AST-semantic + comment check. Both checks deliberately ignore
whitespace and line breaks, so collapsing and wrapping (the Reprinter's whole
job) pass; only real token loss or a comment drop trips them.

## Consequences

- reettier carries a full SWC AST walker (`ast_check.rs`) despite presenting as
  a "layout" tool. This ADR is the answer to "why on earth does it do that?".
- The two checkers are maintained independently. Accepted: they verify the same
  invariant against two very different engines, so a shared abstraction would be
  forced.
