//! The layout-preserving formatting engine for JS/TS token streams.
//!
//! Implements the four rules (see reefmt CONTEXT.md) on top of the lossless
//! tokenizer:
//!   1. Never auto-break — author line breaks outside group boundaries survive.
//!   2. Indentation = bracket-nesting depth (only brackets whose open is
//!      followed by a newline contribute — so hugging is emergent).
//!   3. Trim & Tier-1 spacing + collapse blank lines to one.
//!   4. A group (comma sequence in ()/[]/{}) explodes iff the author broke the
//!      first element/second element boundary; else it collapses. Groups are
//!      independent. reettier manages the trailing comma by shape.

use crate::tokenizer::{tokenize, TokKind};

/// One emitted piece: a significant token (or a synthetic comma) plus the
/// whitespace decision that precedes it.
struct Out {
    text: String,
    kind: TokKind,
    /// 0 = same line, 1 = newline, 2 = one blank line then newline.
    brk: u8,
    /// When `brk == 0`, whether a single space precedes this piece.
    space: bool,
    /// When `brk > 0`, whether this line's indent is structural (group explode /
    /// closing bracket) rather than author-preserved.
    forced: bool,
    /// When `brk > 0` and not `forced`, the author's indent level for this line.
    author_level: usize,
}

struct Frame {
    has_comma: bool,
    has_semicolon: bool,
    first_comma_k: Option<usize>,
    last_comma_k: Option<usize>,
    /// Semicolon positions are tracked for `;`-delimited "groups" (type/interface
    /// members) to apply the same first-boundary switch as comma groups. Only
    /// meaningful when `has_semicolon && !has_comma`.
    first_semicolon_k: Option<usize>,
    last_semicolon_k: Option<usize>,
    force_explode: bool,
    explode: bool,
    close_k: Option<usize>,
    /// Never a comma group regardless of contents — used for CSS `{}` rule
    /// blocks, which are always declaration blocks, never object literals.
    never_group: bool,
    /// A `{}` in statement-block position (after `)`, `=>`, `else`, a bare block,
    /// …). Its `;`s are statement terminators, not member separators, so it is
    /// never a semicolon group — only type/interface literals are.
    stmt_block: bool,
    /// Whether the opening bracket is `{`. Semicolon groups (type/interface member
    /// lists) exist only in braces; `;` inside `()` (a `for` header) or `[]` is a
    /// statement separator, not a member boundary.
    brace: bool,
}

impl Frame {
    fn is_group(&self) -> bool {
        if self.never_group {
            return false;
        }
        // A group is delimited by exactly ONE kind of separator. A frame with both
        // (a multi-declarator `let a = 1, b = 2;` or a sequence expression) is not
        // a group — its comma isn't a member boundary.
        if self.has_comma && self.has_semicolon {
            return false;
        }
        if self.has_comma {
            return true; // comma group (call args, array, object literal)
        }
        // Semicolon group — only for type/interface member lists inside braces,
        // never a statement block (where `;` terminates statements) nor a `for`
        // header `(…;…;…)` / `[]` (where `;` is a statement separator).
        self.has_semicolon && self.brace && !self.stmt_block
    }

    fn is_comma_group(&self) -> bool {
        self.has_comma && !self.has_semicolon && !self.never_group
    }

    fn first_delim_k(&self) -> Option<usize> {
        self.first_comma_k.or(self.first_semicolon_k)
    }
}

/// Whether the `{` at sig index `k` opens a statement block (vs. a type/object
/// literal), based on the preceding significant token. Statement blocks follow
/// `)` (headers), `=>` (arrow bodies), a block keyword, or a bare-block position.
fn is_stmt_block_brace(
    k: usize,
    kind: &dyn Fn(usize) -> TokKind,
    text: &dyn Fn(usize) -> String,
    bchar: &dyn Fn(usize) -> u8,
) -> bool {
    if bchar(k) != b'{' {
        return false; // only `{}` frames can be statement blocks
    }
    if k == 0 {
        return true; // bare block at start of input
    }
    let p = k - 1;
    match kind(p) {
        // `)` → if/for/while/switch/catch header or function body; `}` → block
        // immediately after another block (bare block).
        TokKind::Close => matches!(bchar(p), b')' | b'}'),
        TokKind::Semicolon => true, // bare block after a statement
        TokKind::Open => bchar(p) == b'{', // first thing inside another block
        TokKind::Punct => text(p) == "=>", // arrow function body
        TokKind::Word => matches!(text(p).as_str(), "else" | "do" | "try" | "finally"),
        _ => false,
    }
}

pub fn format_js(src: &str, indent: &str) -> String {
    format_flavored(src, indent, false)
}

pub fn format_css(src: &str, indent: &str) -> String {
    format_flavored(src, indent, true)
}

fn format_flavored(src: &str, indent: &str, css: bool) -> String {
    let out = format_inner(src, indent, css);
    // Self-verify safety net (ADR-0001: the linter owns correctness, but the
    // formatter must never *corrupt*). If our output doesn't preserve the
    // meaning-bearing token stream, we made a mistake — emit the original
    // unchanged so an edge case can never mangle code.
    if crate::tokenizer::signature(src, css) == crate::tokenizer::signature(&out, css) {
        out
    } else {
        if std::env::var("REETTIER_DEBUG").is_ok() {
            eprintln!("reettier: token mismatch — leaving file unchanged");
        }
        src.to_string()
    }
}

fn format_inner(src: &str, indent: &str, css: bool) -> String {
    let tokens = if css { crate::tokenizer::tokenize_css(src) } else { tokenize(src) };

    // ── Significant tokens + the whitespace gap before each ──
    let mut sig: Vec<usize> = Vec::new(); // index into `tokens`
    let mut gap_nl: Vec<usize> = Vec::new(); // # newlines in the gap before this sig
    let mut gap_sp: Vec<bool> = Vec::new(); // any space char in the gap
    let mut gap_indent: Vec<usize> = Vec::new(); // author indent level of this sig's line
    {
        let mut nl = 0usize;
        let mut sp = false;
        let mut indent_ws = String::new(); // whitespace since the last newline
        for (i, t) in tokens.iter().enumerate() {
            match t.kind {
                TokKind::Newline => {
                    nl += 1;
                    indent_ws.clear();
                }
                TokKind::Space => {
                    sp = true;
                    indent_ws = t.text(src).to_string();
                }
                _ => {
                    sig.push(i);
                    gap_nl.push(nl);
                    gap_sp.push(sp);
                    gap_indent.push(author_level(&indent_ws, indent));
                    nl = 0;
                    sp = false;
                    indent_ws.clear();
                }
            }
        }
    }
    let m = sig.len();
    if m == 0 {
        return String::new();
    }

    let kind = |k: usize| tokens[sig[k]].kind;
    let text = |k: usize| tokens[sig[k]].text(src).to_string();
    let bchar = |k: usize| {
        let b = tokens[sig[k]].start;
        src.as_bytes()[b]
    };

    // ── Bracket matching → frames + per-sig role maps ──
    let mut frames: Vec<Frame> = Vec::new();
    let mut open_frame: Vec<Option<usize>> = vec![None; m];
    let mut close_frame: Vec<Option<usize>> = vec![None; m];
    let mut comma_frame: Vec<Option<usize>> = vec![None; m];
    let mut semicolon_frame: Vec<Option<usize>> = vec![None; m];
    // Pass 1: match real brackets `()[]{}` only.
    {
        let mut stack: Vec<usize> = Vec::new();
        for k in 0..m {
            match kind(k) {
                TokKind::Open => {
                    let id = frames.len();
                    frames.push(Frame {
                        has_comma: false,
                        has_semicolon: false,
                        first_comma_k: None,
                        last_comma_k: None,
                        first_semicolon_k: None,
                        last_semicolon_k: None,
                        force_explode: false,
                        explode: false,
                        close_k: None,
                        never_group: css && bchar(k) == b'{',
                        stmt_block: is_stmt_block_brace(k, &kind, &text, &bchar),
                        brace: bchar(k) == b'{',
                    });
                    open_frame[k] = Some(id);
                    stack.push(id);
                }
                TokKind::Close => {
                    if let Some(id) = stack.pop() {
                        close_frame[k] = Some(id);
                        frames[id].close_k = Some(k);
                    }
                }
                _ => {}
            }
        }
    }

    // Pass 2: mark tokens inside generic `<…>` regions, so their commas don't
    // leak into the enclosing bracket's group detection (a `Record<string,
    // unknown>` comma must not make a statement block look like an object).
    // CSS has no generics (and `<`/`>` are combinators), so skip it there.
    let in_generic = if css {
        vec![false; m]
    } else {
        mark_generics(m, &kind, &text, &open_frame, &frames)
    };

    // Pass 3: attribute top-level commas to their frames (skipping generics) and
    // flag frames that hold a line comment (can't be joined across `//`).
    {
        let mut stack: Vec<usize> = Vec::new();
        for k in 0..m {
            match kind(k) {
                TokKind::Open => stack.push(open_frame[k].unwrap()),
                TokKind::Close => {
                    stack.pop();
                }
                TokKind::Comma => {
                    if in_generic[k] {
                        continue;
                    }
                    if let Some(&id) = stack.last() {
                        comma_frame[k] = Some(id);
                        if frames[id].first_comma_k.is_none() {
                            frames[id].first_comma_k = Some(k);
                        }
                        frames[id].last_comma_k = Some(k);
                        frames[id].has_comma = true;
                    }
                }
                TokKind::Semicolon => {
                    if let Some(&id) = stack.last() {
                        frames[id].has_semicolon = true;
                        if frames[id].first_semicolon_k.is_none() {
                            frames[id].first_semicolon_k = Some(k);
                        }
                        frames[id].last_semicolon_k = Some(k);
                        semicolon_frame[k] = Some(id);
                    }
                }
                TokKind::LineComment => {
                    if let Some(&id) = stack.last() {
                        frames[id].force_explode = true;
                    }
                }
                _ => {}
            }
        }
    }

    // ── Explode decision per group: newline at the first element boundary ──
    for f in frames.iter_mut() {
        if !f.is_group() {
            f.explode = false;
            continue;
        }
        // Use the first delimiter (comma or semicolon) to decide the group's shape.
        let fc = f.first_delim_k().unwrap();
        let nl_after = if fc + 1 < m { gap_nl[fc + 1] } else { 0 };
        let nl_before = gap_nl[fc];
        f.explode = f.force_explode || nl_after > 0 || nl_before > 0;
    }

    let explodes = |fid: usize| frames[fid].explode;

    // ── Build the emit list ──
    let mut items: Vec<Out> = Vec::new();
    for k in 0..m {
        // Drop an author trailing comma when its group collapses.
        if let Some(fid) = comma_frame[k] {
            let is_trailing = k + 1 < m && close_frame[k + 1] == Some(fid);
            if is_trailing && !explodes(fid) {
                continue;
            }
        }

        let (brk, space, forced) = decide_gap(
            k,
            &kind,
            &bchar,
            &open_frame,
            &close_frame,
            &comma_frame,
            &semicolon_frame,
            &frames,
            &gap_nl,
            &gap_sp,
        );

        // Insert a synthetic trailing comma for an exploding comma-group. It must go
        // *after the last code token* — before any trailing comment(s) — so it
        // never lands inside a `//` comment, and not if one is already there.
        // Semicolon groups don't get trailing semicolons (they're already terminators).
        // CSS forbids trailing commas (e.g. in rgba()/selector lists), so skip.
        if let Some(fid) = close_frame[k] {
            if !css && frames[fid].is_comma_group() && explodes(fid) && !last_elem_is_rest(&frames[fid], m, &kind, &text) {
                let mut ins = items.len();
                while ins > 0
                    && matches!(items[ins - 1].kind, TokKind::LineComment | TokKind::BlockComment)
                {
                    ins -= 1;
                }
                let already_comma = ins > 0 && items[ins - 1].kind == TokKind::Comma;
                if !already_comma {
                    items.insert(
                        ins,
                        Out {
                            text: ",".to_string(),
                            kind: TokKind::Comma,
                            brk: 0,
                            space: false,
                            forced: false,
                            author_level: 0,
                        },
                    );
                }
            }
        }

        items.push(Out {
            text: text(k),
            kind: kind(k),
            brk,
            space,
            forced,
            author_level: gap_indent[k],
        });
    }

    emit(&items, indent)
}

/// Mark significant tokens that sit inside a generic `<…>` region, so their
/// commas are excluded from group detection. Conservative: a `<` opens a generic
/// only when it follows an identifier/`>` and the interior contains nothing but
/// type-ish tokens up to a balanced `>`. Anything else (a `;`, an operator like
/// `&&`, `=`, `+`) aborts the scan and the `<` is treated as a comparison.
fn mark_generics(
    m: usize,
    kind: &dyn Fn(usize) -> TokKind,
    text: &dyn Fn(usize) -> String,
    open_frame: &[Option<usize>],
    frames: &[Frame],
) -> Vec<bool> {
    let mut in_generic = vec![false; m];
    let mut k = 0;
    while k < m {
        let is_angle_open = kind(k) == TokKind::Punct
            && text(k) == "<"
            && k > 0
            && (kind(k - 1) == TokKind::Word
                || (kind(k - 1) == TokKind::Punct && text(k - 1).bytes().all(|b| b == b'>')));
        if is_angle_open {
            if let Some(close) = scan_generic(k, m, kind, text, open_frame, frames) {
                for x in (k + 1)..close {
                    in_generic[x] = true;
                }
                k = close + 1;
                continue;
            }
        }
        k += 1;
    }
    in_generic
}

/// From an opening `<` at `open`, return the index of the matching `>` if the
/// region is a plausible generic argument list, else `None`.
fn scan_generic(
    open: usize,
    m: usize,
    kind: &dyn Fn(usize) -> TokKind,
    text: &dyn Fn(usize) -> String,
    open_frame: &[Option<usize>],
    frames: &[Frame],
) -> Option<usize> {
    let mut depth: i32 = 0;
    let mut j = open;
    while j < m {
        match kind(j) {
            // Skip a balanced real bracket wholesale (its commas are its own).
            TokKind::Open => {
                let fid = open_frame[j]?;
                j = frames[fid].close_k? + 1;
                continue;
            }
            TokKind::Close => return None,
            TokKind::Semicolon => return None,
            TokKind::Comma | TokKind::Word | TokKind::Str | TokKind::Template
            | TokKind::LineComment | TokKind::BlockComment => {}
            TokKind::Punct => {
                let t = text(j);
                if t == "<" {
                    depth += 1;
                } else if t.bytes().all(|b| b == b'>') {
                    depth -= t.len() as i32;
                    if depth <= 0 {
                        return Some(j);
                    }
                } else if !matches!(
                    t.as_str(),
                    "." | "|" | "&" | "?" | ":" | "=>" | "..." | "[]"
                ) {
                    return None; // an operator → this was a comparison, not a generic
                }
            }
            _ => {}
        }
        j += 1;
    }
    None
}

/// Whether the last element of a frame begins with a spread/rest `...` (in which
/// case a synthetic trailing comma could produce invalid rest syntax).
fn last_elem_is_rest(
    f: &Frame,
    m: usize,
    kind: &dyn Fn(usize) -> TokKind,
    text: &dyn Fn(usize) -> String,
) -> bool {
    if let Some(lc) = f.last_comma_k {
        let start = lc + 1;
        if start < m && kind(start) == TokKind::Punct && text(start) == "..." {
            return true;
        }
    }
    false
}

#[allow(clippy::too_many_arguments)]
fn decide_gap(
    k: usize,
    kind: &dyn Fn(usize) -> TokKind,
    bchar: &dyn Fn(usize) -> u8,
    open_frame: &[Option<usize>],
    close_frame: &[Option<usize>],
    comma_frame: &[Option<usize>],
    semicolon_frame: &[Option<usize>],
    frames: &[Frame],
    gap_nl: &[usize],
    gap_sp: &[bool],
) -> (u8, bool, bool) {
    if k == 0 {
        return (0, false, false);
    }
    let prev = k - 1;
    let explodes = |fid: usize| frames[fid].explode;
    // A single blank line the author placed here survives even at a forced
    // boundary (e.g. blank lines grouping members of an exploded object).
    let blank = if gap_nl[k] >= 2 { 2 } else { 1 };

    // Trailing comment: a comment the author kept on the same line as the
    // preceding code stays attached to it — it is not a new element, so the
    // group-boundary break rules below must not push it onto its own line.
    if matches!(kind(k), TokKind::LineComment | TokKind::BlockComment) && gap_nl[k] == 0 {
        return (0, space_rule(prev, k, kind, bchar, gap_sp), false);
    }

    // A) prev is a *group* open bracket.
    if let Some(fid) = open_frame[prev] {
        if frames[fid].is_group() {
            if explodes(fid) {
                return (blank, false, true);
            }
            // collapse: no break; inner space only for `{`.
            return (0, bchar(prev) == b'{', false);
        }
    }
    // B) cur is a *group* close bracket.
    if let Some(fid) = close_frame[k] {
        if frames[fid].is_group() {
            if explodes(fid) {
                return (1, false, true); // no blank line right before a close
            }
            return (0, bchar(k) == b'}', false);
        }
    }
    // C) prev is a top-level *group* comma (not dropped). A comma in a non-group
    // frame (e.g. a CSS value list inside a rule block) falls through to D/E so
    // the author's layout is preserved.
    if let Some(fid) = comma_frame[prev] {
        if frames[fid].is_group() {
            if explodes(fid) {
                return (blank, false, true);
            }
            return (0, true, false); // one space after comma when inline
        }
    }

    // C') prev is a top-level *group* semicolon. Semicolons in semicolon-delimited
    // groups (type/interface members) explode/collapse the same way commas do.
    if let Some(fid) = semicolon_frame[prev] {
        if frames[fid].is_group() {
            if explodes(fid) {
                return (blank, false, true);
            }
            return (0, true, false); // one space after semicolon when inline
        }
    }

    // C'') prev is a trailing comment that sits between a group delimiter and the
    // next element (e.g. `title: string; /** doc */ prepend?: …`). The comment
    // stays on the previous line (handled above), but the element after it is a
    // new member — so honor the group's explode by breaking here.
    if matches!(kind(prev), TokKind::LineComment | TokKind::BlockComment) {
        let mut p = prev;
        while p > 0 && matches!(kind(p), TokKind::LineComment | TokKind::BlockComment) {
            p -= 1;
        }
        if let Some(fid) = comma_frame[p].or(semicolon_frame[p]) {
            if frames[fid].is_group() && explodes(fid) {
                return (blank, false, true);
            }
        }
    }

    // D/E) Non-boundary: preserve author breaks (Rule 1), else Tier-1 spacing.
    // A single blank line is kept; 2+ collapse to one (per config decision).
    let nl = gap_nl[k];
    if nl > 0 {
        let brk = if nl >= 2 { 2 } else { 1 };
        return (brk, false, false);
    }
    (0, space_rule(prev, k, kind, bchar, gap_sp), false)
}

/// The author's indentation level for a line, measured against the indent unit.
/// For tab indent, counts leading tabs; for space indent, leading spaces divided
/// by the unit width. Trailing alignment spaces are ignored.
fn author_level(ws: &str, indent: &str) -> usize {
    if indent.starts_with('\t') || indent.is_empty() {
        ws.chars().take_while(|c| *c == '\t').count()
    } else {
        let spaces = ws.chars().take_while(|c| *c == ' ').count();
        spaces / indent.len().max(1)
    }
}

/// Tier-1 same-line spacing (Rule 3): punctuation-level only, operator spacing
/// is left to the author.
fn space_rule(
    prev: usize,
    k: usize,
    kind: &dyn Fn(usize) -> TokKind,
    bchar: &dyn Fn(usize) -> u8,
    gap_sp: &[bool],
) -> bool {
    let pk = kind(prev);
    let ck = kind(k);

    // Empty brackets: `()`, `[]`, `{}` — never an inner space.
    if pk == TokKind::Open && ck == TokKind::Close {
        return false;
    }
    // No space before a comma/semicolon.
    if ck == TokKind::Comma || ck == TokKind::Semicolon {
        return false;
    }
    // One space after a comma/semicolon — but not right before a closing bracket
    // (e.g. `for (;;)`, or a dropped trailing comma). A trailing `;` before `}` is
    // handled by the brace rule below, which still gives `{ a; }` its inner space.
    if (pk == TokKind::Comma || pk == TokKind::Semicolon) && ck != TokKind::Close {
        return true;
    }
    // Inside brackets: braces get an inner space, `(`/`[` do not.
    if pk == TokKind::Open {
        return bchar(prev) == b'{';
    }
    if ck == TokKind::Close {
        return bchar(k) == b'}';
    }
    // Default: preserve whether the author had whitespace (collapsed to one).
    gap_sp[k]
}

/// Emit the pieces with base+relative indentation.
///
/// A bracket that indents (its open is followed by a newline) sets its content
/// level to `its own line's level + 1`. Forced lines (group explode, closing
/// brackets) take that structural level exactly; author-preserved lines take
/// `max(structural, author_level)` so extra indentation the author added for
/// `case:` bodies, method chains, and labels survives.
#[allow(unused_assignments)] // at_line_start is written on the last iteration
fn emit(items: &[Out], indent: &str) -> String {
    struct Bracket {
        indents: bool,
        open_line_level: usize,
    }
    let mut out = String::new();
    let mut stack: Vec<Bracket> = Vec::new();
    // Content levels of currently-open *indenting* brackets.
    let mut indent_stack: Vec<usize> = Vec::new();
    let mut pending_open: Option<usize> = None;
    let mut cur_line_level: usize = 0;
    let mut at_line_start = true;

    for it in items {
        // Resolve the previous open's indent decision from whether we break now.
        if let Some(idx) = pending_open.take() {
            if it.brk > 0 {
                stack[idx].indents = true;
                indent_stack.push(stack[idx].open_line_level + 1);
            }
        }

        if it.brk > 0 {
            if it.brk == 2 {
                out.push('\n');
            }
            out.push('\n');
            let structural = *indent_stack.last().unwrap_or(&0);
            let level = if it.kind == TokKind::Close {
                // Dedent to the matching open's line level.
                stack.last().map(|b| b.open_line_level).unwrap_or(0)
            } else if it.forced {
                structural
            } else {
                structural.max(it.author_level)
            };
            cur_line_level = level;
            for _ in 0..level {
                out.push_str(indent);
            }
            at_line_start = true;
        } else if it.space && !at_line_start {
            out.push(' ');
        }

        out.push_str(&it.text);
        at_line_start = false;

        match it.kind {
            TokKind::Open => {
                stack.push(Bracket {
                    indents: false,
                    open_line_level: cur_line_level,
                });
                pending_open = Some(stack.len() - 1);
            }
            TokKind::Close => {
                if let Some(b) = stack.pop() {
                    if b.indents {
                        indent_stack.pop();
                    }
                }
            }
            _ => {}
        }
    }

    // File-level cleanup: trim leading blank lines, ensure single trailing newline.
    let trimmed = out.trim_start_matches('\n');
    let mut result = trimmed.trim_end().to_string();
    if !result.is_empty() {
        result.push('\n');
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fmt(s: &str) -> String {
        format_js(s, "\t")
    }
    /// Formatting must be idempotent.
    fn assert_idempotent(s: &str) {
        let once = fmt(s);
        let twice = fmt(&once);
        assert_eq!(once, twice, "not idempotent:\n---once---\n{once}\n---twice---\n{twice}");
    }

    #[test]
    fn collapse_pulls_up_trailing_element() {
        // Grill Q1 canonical example.
        let input = "foo(a, b,\n    c)\n";
        assert_eq!(fmt(input), "foo(a, b, c)\n");
        assert_idempotent(input);
    }

    #[test]
    fn explode_when_first_boundary_broken() {
        let input = "foo(a,\n b)\n";
        assert_eq!(fmt(input), "foo(\n\ta,\n\tb,\n)\n");
        assert_idempotent(input);
    }

    #[test]
    fn single_element_never_explodes() {
        // No second element → no boundary → stays inline however long.
        let input = "foo(reallyLongSingleArgumentThatWouldWrapInPrettier)\n";
        assert_eq!(fmt(input), input);
        assert_idempotent(input);
    }

    #[test]
    fn nested_hug() {
        // Collapsed outer wrapping an exploded inner → closers hug.
        let input = "foo(a, bar(\n\tb,\n\tc\n))\n";
        let out = fmt(input);
        assert_eq!(out, "foo(a, bar(\n\tb,\n\tc,\n))\n");
        assert_idempotent(input);
    }

    #[test]
    fn block_indents_but_is_not_a_group() {
        let input = "function f() {\nreturn 1\n}\n";
        assert_eq!(fmt(input), "function f() {\n\treturn 1\n}\n");
        assert_idempotent(input);
    }

    #[test]
    fn arrow_arg_hugs_without_special_case() {
        let input = "foo(() => {\nbar()\n})\n";
        assert_eq!(fmt(input), "foo(() => {\n\tbar()\n})\n");
        assert_idempotent(input);
    }

    #[test]
    fn empty_brackets_no_inner_space() {
        assert_eq!(fmt("f()\n"), "f()\n");
        assert_eq!(fmt("x = []\n"), "x = []\n");
        assert_eq!(fmt("y = {}\n"), "y = {}\n");
    }

    #[test]
    fn tier1_spacing_and_blank_collapse() {
        assert_eq!(fmt("foo( a ,b )\n"), "foo(a, b)\n");
        assert_eq!(fmt("a\n\n\n\nb\n"), "a\n\nb\n");
    }

    #[test]
    fn strings_are_untouched() {
        let input = "const s = \"a,  b   c\";\n";
        assert_eq!(fmt(input), input);
    }

    #[test]
    fn operator_spacing_preserved() {
        // Rule 3 does not touch operator spacing.
        assert_eq!(fmt("a+b\n"), "a+b\n");
        assert_eq!(fmt("a  +  b\n"), "a + b\n"); // multi-space collapses to one
    }

    #[test]
    fn line_comment_in_group_forces_explode() {
        let input = "foo(a, // note\nb)\n";
        let out = fmt(input);
        assert!(out.contains("// note"), "comment kept: {out}");
        assert_idempotent(input);
    }

    #[test]
    fn nested_object_in_call_collapses_independently() {
        let input = "fn({ a: 1 })\n";
        assert_eq!(fmt(input), "fn({ a: 1 })\n");
        assert_idempotent(input);
    }

    #[test]
    fn trailing_line_comment_stays_on_its_line() {
        // Regression: a trailing `//` comment must not be pushed to its own line.
        let input = "const o = {\n\tpath_segment: \"\", // /blog/2/ note\n\tx: 1,\n}\n";
        let out = fmt(input);
        assert!(
            out.contains("path_segment: \"\", // /blog/2/ note"),
            "trailing comment split off:\n{out}"
        );
        assert_idempotent(input);
    }

    #[test]
    fn generic_comma_does_not_collapse_block() {
        // Regression: the comma in Record<string, unknown> must not make the
        // statement block look like a collapsible object literal.
        let input = "if (x) {\n\tconst obj = entry as Record<string, unknown>;\n\tconst name = obj.name;\n}\n";
        let out = fmt(input);
        assert_eq!(out, input, "block wrongly collapsed:\n{out}");
        assert_idempotent(input);
    }

    #[test]
    fn generic_comma_does_not_split_object_member() {
        let input = "const o = {\n\tfn: cast<A, B>(),\n}\n";
        let out = fmt(input);
        // The generic comma must not become an element boundary.
        assert!(out.contains("cast<A, B>()"), "generic split:\n{out}");
        assert_idempotent(input);
    }

    fn css(s: &str) -> String {
        format_css(s, "\t")
    }

    #[test]
    fn css_indents_by_block_nesting() {
        let input = "@media (min-width: 700px) {\n.baz {\ncolor: red;\n}\n}\n";
        assert_eq!(css(input), "@media (min-width: 700px) {\n\t.baz {\n\t\tcolor: red;\n\t}\n}\n");
        assert_eq!(css(&css(input)), css(input));
    }

    #[test]
    fn css_selector_and_function_comma_spacing() {
        assert_eq!(css(".a,.b { color: red }\n"), ".a, .b { color: red }\n");
        assert_eq!(css("x { c: rgba(0,0,0,.5) }\n"), "x { c: rgba(0, 0, 0, .5) }\n");
    }

    #[test]
    fn css_preserves_blank_lines_and_value_lists() {
        let input = ".a { x: 1 }\n\n.b {\n\tfont: a,\n\t\tb,\n\t\tc;\n}\n";
        // Value list (comma, no bracket) is preserved verbatim, not exploded.
        assert_eq!(css(input), input);
    }

    #[test]
    fn css_no_line_comments_or_regex() {
        // `//` is not a comment in CSS; `/` in shorthand is just punctuation.
        let input = "a { font: 16px/1.5 sans-serif; background: url(http://x/y) }\n";
        assert_eq!(css(input), input);
    }

    #[test]
    fn real_comparison_is_not_a_generic() {
        // `a < b` must stay a comparison; the group comma still works.
        let input = "foo(a < b, c > d)\n";
        assert_idempotent(input);
    }

    #[test]
    fn multi_declarator_is_not_a_group() {
        // Regression: `let a = 0, b = 0;`'s comma must not make the enclosing
        // block a group (which added a stray trailing comma each pass).
        let input = "while (x) {\n\tlet a = 0, b = 0, c = 0;\n\treturn null; // no closing paren\n}\n";
        let out = fmt(input);
        assert_eq!(out, input, "block wrongly treated as group:\n{out}");
        assert_idempotent(input);
    }

    #[test]
    fn semicolon_type_literal_explodes_on_first_boundary() {
        // `;`-separated type members follow the same first-boundary switch as `,`.
        let input = "type T = { a: string;\nb: number; c: boolean; }\n";
        assert_eq!(
            fmt(input),
            "type T = {\n\ta: string;\n\tb: number;\n\tc: boolean;\n}\n"
        );
        assert_idempotent(&fmt(input));
    }

    #[test]
    fn semicolon_type_literal_collapses_when_inline() {
        let input = "type T = { a: string; b: number; };\n";
        assert_eq!(fmt(input), "type T = { a: string; b: number; };\n");
    }

    #[test]
    fn semicolon_group_no_trailing_comma() {
        // A semicolon group must not get a synthetic trailing comma on explode.
        let out = fmt("interface I { a: string;\nb: number; }\n");
        assert!(!out.contains(","), "spurious comma:\n{out}");
    }

    #[test]
    fn exploded_member_breaks_after_trailing_comment() {
        // A trailing comment stays on the member's line; the next member still
        // breaks onto its own line.
        let input = "type T = { a: string;\nb: string; /** doc */ c: string; }\n";
        assert_eq!(
            fmt(input),
            "type T = {\n\ta: string;\n\tb: string; /** doc */\n\tc: string;\n}\n"
        );
    }

    #[test]
    fn statement_block_is_not_a_semicolon_group() {
        // A block after `)` / `=>` must never collapse its statements onto one line.
        let arrow = "const f = () => {\n\tdoA(); doB();\n};\n";
        assert_eq!(fmt(arrow), arrow, "arrow body collapsed");
        let if_body = "if (x) {\n\tif (y) a++; else b++;\n}\n";
        assert_eq!(fmt(if_body), if_body, "if body collapsed");
    }

    #[test]
    fn for_header_is_not_a_semicolon_group() {
        // The `;`s in a `for (…;…;…)` header are statement separators, not member
        // boundaries — the header must never explode one-clause-per-line.
        let compact = "for (let i = 0; i < n; i++) {\n\tx();\n}\n";
        assert_eq!(fmt(compact), compact, "compact for-header changed");
        // Author-broken header is preserved (never auto-collapse or explode).
        let broken = "for (let i = 0;\ni < n; i++) {\n\tx();\n}\n";
        let out = fmt(broken);
        assert!(!out.contains("let i = 0;\n\ti < n;\n"), "for-header exploded:\n{out}");
        assert_idempotent(broken);
        // Empty header: no space injected before the `)`.
        let empty = "for (;;) {\n\tx();\n}\n";
        assert_eq!(fmt(empty), empty, "for(;;) got a stray space");
    }

    #[test]
    fn trailing_semicolon_keeps_brace_inner_space() {
        // A `;` right before `}` still yields the brace's inner space, not `;)`.
        assert_eq!(fmt("type T = { a: string; };\n"), "type T = { a: string; };\n");
    }

    #[test]
    fn switch_case_bodies_keep_their_indent() {
        let input = "switch (x) {\n\tcase 1:\n\t\treturn a;\n\tdefault:\n\t\treturn b;\n}\n";
        assert_eq!(fmt(input), input, "case body de-indented");
        assert_idempotent(input);
    }

    #[test]
    fn method_chain_keeps_indent() {
        let input = "const s = text\n\t.toLowerCase()\n\t.trim();\n";
        assert_eq!(fmt(input), input, "chain de-indented");
        assert_idempotent(input);
    }

    #[test]
    fn blank_lines_inside_exploded_object_survive() {
        let input = "const O = {\n\ta: 1,\n\n\t// group two\n\tb: 2,\n}\n";
        let out = fmt(input);
        assert!(out.contains("a: 1,\n\n\t// group two"), "blank line lost:\n{out}");
        assert_idempotent(input);
    }

    #[test]
    fn synthetic_comma_goes_before_trailing_comment() {
        // Regression: exploded group whose last element has a trailing comment —
        // the managed comma must land after the code, not inside the comment.
        let input = "foo(\n\ta,\n\tb // last\n)\n";
        let out = fmt(input);
        assert!(out.contains("b, // last"), "comma misplaced:\n{out}");
        assert!(!out.contains("// last,"), "comma entered comment:\n{out}");
        assert_idempotent(input);
    }
}
