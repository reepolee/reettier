//! `.ree` template formatting.
//!
//! Two concerns, composed:
//!   1. **Embedded code** — raw JS `{{ … }}`, `<script>` JS, `<style>` CSS are
//!      reformatted by the JS/CSS engines and re-indented to their base column.
//!      `<pre>`/`<textarea>`/`<!-- -->` are kept verbatim.
//!   2. **Markup indentation** — every line is re-indented to its HTML-tag and
//!      Ree-directive structural nesting depth (Rule 2), correcting both under-
//!      and over-indentation. The author indent is used only as a floor for
//!      broken-tag attribute continuations, which have no structural signal.
//!
//! Implementation: mask each embedded/verbatim block to a one-line placeholder,
//! indent the resulting markup line-by-line, then expand the placeholders at the
//! corrected base indent.
//!
//! Guarded by a whitespace/comma-insensitive safety net: any change beyond
//! whitespace and (managed) commas causes the original to be emitted unchanged.

use crate::engine::{format_css, format_js};
use crate::tokenizer::{tokenize, TokKind};

const VOID: &[&str] = &[
    "area", "base", "br", "col", "embed", "hr", "img", "input", "link", "meta",
    "param", "source", "track", "wbr",
];

/// Ree block directives that open a nesting level (matched by `{/kw}`).
const REE_BLOCK_OPEN: &[&str] = &["if", "each", "with", "for"];

#[allow(dead_code)]
pub fn format_ree(src: &str, indent: &str) -> String {
    format_ree_with_options(src, indent, false, 100)
}

pub fn format_ree_with_options(src: &str, indent: &str, wrap_markup: bool, wrap_width: usize) -> String {
    let out = format_ree_inner(src, indent, wrap_markup, wrap_width);
    if strip(src) == strip(&out) {
        out
    } else {
        if std::env::var("REETTIER_DEBUG").is_ok() {
            eprintln!("reettier: .ree content mismatch — leaving file unchanged");
        }
        src.to_string()
    }
}

fn strip(s: &str) -> String {
    s.chars()
        .filter(|c| !c.is_ascii_whitespace() && *c != ',')
        .collect()
}

fn format_ree_inner(src: &str, indent: &str, wrap_markup: bool, wrap_width: usize) -> String {
    let (masked, blocks) = extract_blocks(src, indent);
    let wrapped = if wrap_markup { wrap_markup_lines(&masked, wrap_width) } else { masked };
    indent_markup(&wrapped, indent, &blocks)
}

fn wrap_markup_lines(src: &str, width: usize) -> String {
    if width == 0 { return src.to_string(); }
    let mut out = String::new();
    for (line_index, line) in src.split('\n').enumerate() {
        if line_index > 0 { out.push('\n'); }
        if line.len() <= width { out.push_str(line); continue; }
        let mut start = 0usize;
        let bytes = line.as_bytes();
        let mut quote = None;
        for i in 0..bytes.len() {
            let ch = bytes[i] as char;
            if quote == Some(ch) { quote = None; } else if quote.is_none() && (ch == '"' || ch == '\'') { quote = Some(ch); }
            let is_opening_tag = ch == '<' && i + 1 < bytes.len() && bytes[i + 1] != b'/';
            if quote.is_none() && is_opening_tag && i > start && line[start..i].trim().len() > 0 {
                out.push_str(line[start..i].trim_end());
                out.push('\n');
                start = i;
            }
        }
        out.push_str(line[start..].trim_start());
    }
    out
}

// ── Block extraction / masking ───────────────────────────────────────────

enum Block {
    /// Reformatted code: rendered as `open` line, `body` at base+indent, `close`.
    Code { open: String, body: Vec<String>, close: String },
    /// Verbatim text (pre/textarea/comment/inline): emitted unchanged.
    Verbatim(String),
}

const PH_START: char = '\u{E000}';
const PH_END: char = '\u{E001}';

fn placeholder(idx: usize) -> String {
    format!("{}{}{}", PH_START, idx, PH_END)
}

/// Replace each multi-line embedded/verbatim block with a one-line placeholder.
fn extract_blocks(src: &str, indent: &str) -> (String, Vec<Block>) {
    let b = src.as_bytes();
    let n = b.len();
    let mut masked = String::new();
    let mut blocks: Vec<Block> = Vec::new();
    let mut i = 0;
    // An opening fence glued to preceding content on its line (e.g.
    // `<md-text type="code">```ts`, no newline before the backticks) is not
    // masked below — `fence_here` only opens at true line start, since a
    // glued opener's *own* content is markup-adjacent text, not a clean
    // verbatim span. But its closer is a normal own-line ``` and would
    // otherwise be misread as the start of a brand new fence, swallowing
    // everything up to the *next* accidental closer. Tracking "currently past
    // a glued opener, mark X, run length N" lets the real closer be recognized
    // and passed through as plain text instead.
    let mut glued_fence: Option<(u8, usize)> = None;

    while i < n {
        // Verbatim: <pre>, <textarea>, HTML comment.
        if src[i..].starts_with("<!--") {
            let end = src[i..].find("-->").map(|p| i + p + 3).unwrap_or(n);
            push_block(&mut masked, &mut blocks, Block::Verbatim(src[i..end].to_string()));
            i = end;
            continue;
        }
        if let Some(tag) = verbatim_tag_here(src, i) {
            if let Some((_, _, after)) = element_content(src, i, tag) {
                push_block(&mut masked, &mut blocks, Block::Verbatim(src[i..after].to_string()));
                i = after;
                continue;
            }
        }
        // Fenced code block (```/~~~), wherever it appears in markup — most
        // commonly a code sample inside <md-text type="code"> or a component
        // slot. See `fence_here` for why this must be opaque to tag scanning.
        if b[i] == b'`' || b[i] == b'~' {
            if let Some((mark, run_len)) = glued_fence {
                if b[i] == mark && closer_run_here(src, i, mark, run_len) {
                    glued_fence = None;
                    // Fall through to the generic copy below: pass the
                    // closer through as plain text, matching how the glued
                    // opener itself was left untouched.
                } else if at_line_start(src, i) {
                    if let Some(end) = fence_here(src, i) {
                        push_block(&mut masked, &mut blocks, Block::Verbatim(src[i..end].to_string()));
                        i = end;
                        continue;
                    }
                }
            } else if at_line_start(src, i) {
                if let Some(end) = fence_here(src, i) {
                    push_block(&mut masked, &mut blocks, Block::Verbatim(src[i..end].to_string()));
                    i = end;
                    continue;
                }
            } else if let Some(run_len) = fence_run_len_here(src, i) {
                // Glued opener: record it and fall through untouched.
                glued_fence = Some((b[i], run_len));
            }
        }
        // Raw JS block: {{ … }}
        if src[i..].starts_with("{{") {
            if let Some((inner_s, inner_e, after)) = find_rawjs_end(src, i) {
                if let Some(block) = code_block(src, i, inner_s, inner_e, after, indent, Lang::Js) {
                    push_block(&mut masked, &mut blocks, block);
                    i = after;
                    continue;
                }
            }
        }
        // <script>/<style>
        if let Some((tag, lang)) = open_tag_here(src, i) {
            if let Some((cs, ce, after)) = element_content(src, i, tag) {
                // A <script> with a non-JS `type` (JSON, speculationrules,
                // importmap, text/template, …) is data, not code — keep verbatim
                // so we never inject a JSON-breaking trailing comma.
                let is_js_script = tag != "script" || script_is_js(&src[i..cs]);
                if is_js_script {
                    if let Some(block) = code_block(src, i, cs, ce, after, indent, lang) {
                        push_block(&mut masked, &mut blocks, block);
                        i = after;
                        continue;
                    }
                }
                // Empty/inline element → verbatim.
                push_block(&mut masked, &mut blocks, Block::Verbatim(src[i..after].to_string()));
                i = after;
                continue;
            }
        }
        let l = utf8_len(b[i]);
        masked.push_str(&src[i..i + l]);
        i += l;
    }
    (masked, blocks)
}

fn push_block(masked: &mut String, blocks: &mut Vec<Block>, block: Block) {
    masked.push_str(&placeholder(blocks.len()));
    blocks.push(block);
}

#[derive(Clone, Copy)]
enum Lang {
    Js,
    Css,
}

/// Build a `Code` block for a multi-line embedded region, or `None` (→ verbatim)
/// when it's inline/empty.
fn code_block(
    src: &str,
    open: usize,
    inner_s: usize,
    inner_e: usize,
    after: usize,
    indent: &str,
    lang: Lang,
) -> Option<Block> {
    let inner = &src[inner_s..inner_e];
    if !inner.contains('\n') || inner.trim().is_empty() {
        return None;
    }
    // Ree directives ({#each}, {~ x}, {/each}, …) mixed into the code aren't
    // valid JS/CSS — formatting would mangle them. If any appear outside string
    // literals, leave the whole block verbatim. (Interpolations inside strings,
    // e.g. "{~ localized_path() }", are fine and still get formatted.)
    if has_ree_directive(inner, matches!(lang, Lang::Css)) {
        return None;
    }
    let dedented = dedent(inner);
    let formatted = match lang {
        Lang::Js => format_js(&dedented, indent),
        Lang::Css => format_css(&dedented, indent),
    };
    let body: Vec<String> = formatted
        .trim_end_matches('\n')
        .lines()
        .map(|l| l.to_string())
        .collect();
    Some(Block::Code {
        open: src[open..inner_s].trim_end().to_string(),
        body,
        close: src[inner_e..after].trim_start().to_string(),
    })
}

/// Whether the code contains a bare Ree directive — a `{` immediately followed
/// by one of `# / : = ~ - _` outside any string/template/comment. Such a block
/// is left verbatim rather than formatted as JS/CSS.
fn has_ree_directive(content: &str, css: bool) -> bool {
    let toks = if css {
        crate::tokenizer::tokenize_css(content)
    } else {
        tokenize(content)
    };
    let sig: Vec<_> = toks
        .iter()
        .filter(|t| !matches!(t.kind, TokKind::Space | TokKind::Newline))
        .collect();
    for w in sig.windows(2) {
        let is_brace = w[0].kind == TokKind::Open && content.as_bytes()[w[0].start] == b'{';
        if is_brace {
            let first = content.as_bytes()[w[1].start];
            if matches!(first, b'#' | b'/' | b':' | b'=' | b'~' | b'-' | b'_') {
                return true;
            }
        }
    }
    false
}

// ── Markup line indentation ──────────────────────────────────────────────

fn indent_markup(masked: &str, indent: &str, blocks: &[Block]) -> String {
    let mut out = String::new();
    let mut depth: i32 = 0;
    // State for an open tag whose `>` is on a later line (broken attributes).
    let mut in_tag = false;
    let mut tag_base = 0usize;
    let mut tag_opener = false;

    let lines: Vec<&str> = masked.split('\n').collect();
    for (li, raw) in lines.iter().enumerate() {
        let author_ws: String = raw.chars().take_while(|c| *c == ' ' || *c == '\t').collect();
        let author = author_level(&author_ws, indent);
        // Tier-1 "trim & space": strip trailing whitespace, then collapse
        // inter-tag whitespace runs on the line. (Leading whitespace is the
        // author indent, re-applied structurally below.)
        let collapsed = collapse_inter_tag(raw.trim());
        let trimmed = collapsed.as_str();

        if trimmed.is_empty() {
            // keep blank line
        } else if in_tag {
            // Continuation of a multi-line open tag: attributes indent one level
            // under the tag; a lone `>` / `/>` aligns with the tag.
            let just_close = trimmed == ">" || trimmed == "/>";
            let level = if just_close { tag_base } else { tag_base + 1 }.max(author);
            let base = indent.repeat(level);
            render_line(&mut out, trimmed, &base, indent, blocks);
            if let Some(self_closing) = tag_closes_here(trimmed) {
                in_tag = false;
                if tag_opener && !self_closing {
                    depth = tag_base as i32 + 1;
                }
            }
        } else {
            let (head, trailing) = split_trailing_closers(trimmed);
            // Analyze the *head* only. When there is nothing to split,
            // `head == trimmed`, so this is identical to the old behaviour.
            let (leading_closers, opens, closes, pending) = analyze_line(head);
            // Rule 2: indentation = structural nesting depth, re-applied. The
            // structural level wins in both directions — over-indented lines are
            // pulled back in, not preserved. (`author` is still the floor for
            // broken-tag attribute continuations in the `in_tag` branch, which
            // have no structural signal of their own.)
            let line_level = (depth - leading_closers as i32).max(0) as usize;
            let level = line_level;
            let base = indent.repeat(level);

            if trailing.is_empty() || pending.is_some() {
                // Unchanged path: render the whole line as one unit.
                render_line(&mut out, trimmed, &base, indent, blocks);
                depth = (depth + opens as i32 - closes as i32).max(0);
                if let Some(is_opener) = pending {
                    in_tag = true;
                    tag_base = level;
                    tag_opener = is_opener;
                }
            } else {
                // Render the content head, then peel each trailing block-closer
                // onto its own line, dedenting one level per closer relative to
                // the head's rendered level.
                render_line(&mut out, head, &base, indent, blocks);
                depth = (depth + opens as i32 - closes as i32).max(0);
                for (k, closer) in trailing.iter().enumerate() {
                    let closer_level = level.saturating_sub(1 + k);
                    let cbase = indent.repeat(closer_level);
                    out.push('\n');
                    render_line(&mut out, closer, &cbase, indent, blocks);
                    depth = (depth - 1).max(0);
                }
            }
        }

        if li + 1 < lines.len() {
            out.push('\n');
        }
    }

    // Trailing newline normalization.
    let trimmed = out.trim_end_matches('\n');
    let mut result = trimmed.to_string();
    if !result.is_empty() {
        result.push('\n');
    }
    result
}

/// Emit one line at `base` indent, expanding every embedded placeholder in place.
/// Text segments are emitted as-is; a `Code` block placeholder opens a multi-line
/// region (body at `base+indent`, close at `base`); a `Verbatim` placeholder is
/// inlined.
fn render_line(out: &mut String, trimmed: &str, base: &str, indent: &str, blocks: &[Block]) {
    out.push_str(base);
    let mut rest = trimmed;
    while let Some(sp) = rest.find(PH_START) {
        out.push_str(&rest[..sp]);
        let after = &rest[sp + PH_START.len_utf8()..];
        if let Some(ep) = after.find(PH_END) {
            if let Ok(idx) = after[..ep].parse::<usize>() {
                match &blocks[idx] {
                    Block::Code { open, body, close } => {
                        out.push_str(open);
                        for l in body {
                            out.push('\n');
                            if !l.is_empty() {
                                out.push_str(base);
                                out.push_str(indent);
                                out.push_str(l);
                            }
                        }
                        out.push('\n');
                        out.push_str(base);
                        out.push_str(close);
                    }
                    Block::Verbatim(text) => out.push_str(text),
                }
            }
            rest = &after[ep + PH_END.len_utf8()..];
        } else {
            out.push_str(&rest[sp..]);
            return;
        }
    }
    out.push_str(rest);
}

/// Count structural depth effects of a markup line: `(leading_closers, opens,
/// closes, pending)`. `pending` is `Some(is_opener)` when the line ends inside an
/// unclosed `<tag` (broken attributes). Leading closers dedent the line itself.
fn analyze_line(line: &str) -> (usize, usize, usize, Option<bool>) {
    let b = line.as_bytes();
    let n = b.len();
    let mut i = 0;
    let mut opens = 0usize;
    let mut closes = 0usize;
    let mut leading_closers = 0usize;
    let mut seen_opener = false;
    let mut pending: Option<bool> = None;

    while i < n {
        // HTML tags
        if b[i] == b'<' {
            if i + 1 < n && b[i + 1] == b'/' {
                closes += 1;
                if !seen_opener {
                    leading_closers += 1;
                }
                i = tag_end(line, i);
                continue;
            }
            if i + 1 < n && (b[i + 1].is_ascii_alphabetic()) {
                let end = tag_end(line, i);
                let closed = end > i && b[end - 1] == b'>';
                let name = tag_name(&line[i..end]);
                if !closed {
                    // Tag continues on the next line.
                    pending = Some(!VOID.contains(&name.as_str()));
                    break;
                }
                let self_closing = line[i..end].trim_end().ends_with("/>");
                if !self_closing && !VOID.contains(&name.as_str()) {
                    opens += 1;
                    seen_opener = true;
                }
                i = end;
                continue;
            }
        }
        // Ree directives
        if b[i] == b'{' && i + 1 < n {
            match b[i + 1] {
                b'#' => {
                    let kw = ree_keyword(&line[i..]);
                    if REE_BLOCK_OPEN.contains(&kw.as_str()) {
                        opens += 1;
                        seen_opener = true;
                    }
                    i = brace_end(line, i);
                    continue;
                }
                b'/' => {
                    closes += 1;
                    if !seen_opener {
                        leading_closers += 1;
                    }
                    i = brace_end(line, i);
                    continue;
                }
                b':' => {
                    // {:else} / {:else if} — dedent this line, net zero.
                    if !seen_opener {
                        leading_closers += 1;
                    }
                    i = brace_end(line, i);
                    continue;
                }
                _ => {
                    i = brace_end(line, i);
                    continue;
                }
            }
        }
        i += utf8_len(b[i]);
    }
    (leading_closers, opens, closes, pending)
}

/// Trailing structural block-closers on a line: ree `{/kw}` or non-void HTML
/// `</tag>` closers that are **not** matched by an opener earlier on the same
/// line (they close a block opened on a previous line) and sit at the **tail**
/// of the line (only whitespace / further such closers follow).
///
/// Returns `(head, closers)`: `head` is the content before the trailing run,
/// `closers` each trailing closer in source order. `closers` is empty when
/// there is nothing to split — no trailing structural closer, or no
/// non-whitespace content precedes it (a lone closer, which the leading-closer
/// path already handles correctly).
fn split_trailing_closers(line: &str) -> (&str, Vec<&str>) {
    let b = line.as_bytes();
    let n = b.len();
    let mut i = 0usize;
    let mut local_depth: i32 = 0; // same-line openers still open
    let mut run_start: Option<usize> = None;
    let mut closers: Vec<(usize, usize)> = Vec::new();

    while i < n {
        let c = b[i];

        // Whitespace never breaks a trailing run.
        if c == b' ' || c == b'\t' {
            i += 1;
            continue;
        }

        // HTML close tag `</name>`
        if c == b'<' && i + 1 < n && b[i + 1] == b'/' {
            let end = tag_end(line, i);
            let name = tag_name(&line[i..end]);
            if VOID.contains(&name.as_str()) {
                run_start = None;
                closers.clear();
            } else if local_depth > 0 {
                // Matched by a same-line opener → content, breaks the run.
                local_depth -= 1;
                run_start = None;
                closers.clear();
            } else {
                // Structural closer (opened on a previous line).
                if run_start.is_none() {
                    run_start = Some(i);
                }
                closers.push((i, end));
            }
            i = end;
            continue;
        }

        // HTML open tag `<name …>`
        if c == b'<' && i + 1 < n && b[i + 1].is_ascii_alphabetic() {
            let end = tag_end(line, i);
            let closed = end > i && b[end - 1] == b'>';
            let name = tag_name(&line[i..end]);
            if closed {
                let self_closing = line[i..end].trim_end().ends_with("/>");
                if !self_closing && !VOID.contains(&name.as_str()) {
                    local_depth += 1;
                }
            }
            run_start = None;
            closers.clear();
            i = end;
            continue;
        }

        // Ree directive `{…}`
        if c == b'{' && i + 1 < n {
            match b[i + 1] {
                b'#' => {
                    let kw = ree_keyword(&line[i..]);
                    if REE_BLOCK_OPEN.contains(&kw.as_str()) {
                        local_depth += 1;
                    }
                    run_start = None;
                    closers.clear();
                    i = brace_end(line, i);
                    continue;
                }
                b'/' => {
                    let end = brace_end(line, i);
                    if local_depth > 0 {
                        local_depth -= 1;
                        run_start = None;
                        closers.clear();
                    } else {
                        if run_start.is_none() {
                            run_start = Some(i);
                        }
                        closers.push((i, end));
                    }
                    i = end;
                    continue;
                }
                // `{~ x}`, `{:else}`, `{ text }` — content, breaks the run.
                _ => {
                    run_start = None;
                    closers.clear();
                    i = brace_end(line, i);
                    continue;
                }
            }
        }

        // Any other visible char is content → breaks a trailing run.
        run_start = None;
        closers.clear();
        i += utf8_len(c);
    }

    match run_start {
        Some(s) => {
            let head = line[..s].trim_end();
            if head.is_empty() {
                // Lone closer(s): defer to the existing leading-closer path.
                (line, Vec::new())
            } else {
                let slices = closers.iter().map(|&(a, e)| line[a..e].trim()).collect();
                (head, slices)
            }
        }
        None => (line, Vec::new()),
    }
}

/// Collapse a run of whitespace that sits **between** two markup tokens — an
/// HTML tag or ree directive close (`>` / `}`) on the left and an open (`<` /
/// `{`) on the right — down to a single space. This is the inter-tag part of
/// Tier-1 "trim & space": HTML renders any run of ASCII whitespace as a single
/// space, so this is a whitespace-only, render-preserving normalization.
///
/// Whitespace inside tags, directives, and quoted attributes is skipped (tags
/// and directives are copied as opaque units via `tag_end` / `brace_end`), and
/// any run that touches a non-tag character (text, or a masked-block
/// placeholder) is left exactly as-is — so text content and embedded blocks are
/// never disturbed.
fn collapse_inter_tag(line: &str) -> String {
    let b = line.as_bytes();
    let n = b.len();
    let mut out = String::with_capacity(n);
    let mut i = 0usize;
    let mut prev_is_close = false; // last emitted token ended with `>` or `}`

    while i < n {
        let c = b[i];

        // HTML tag `<…>` or `</…>` — copy verbatim (quotes handled by tag_end).
        if c == b'<' && i + 1 < n && (b[i + 1] == b'/' || b[i + 1].is_ascii_alphabetic()) {
            let end = tag_end(line, i);
            out.push_str(&line[i..end]);
            prev_is_close = b[end - 1] == b'>';
            i = end;
            continue;
        }

        // Ree directive `{…}` — copy verbatim (quotes handled by brace_end).
        if c == b'{' && i + 1 < n {
            let end = brace_end(line, i);
            out.push_str(&line[i..end]);
            prev_is_close = b[end - 1] == b'}';
            i = end;
            continue;
        }

        // Whitespace run: collapse only when framed by close→open.
        if c == b' ' || c == b'\t' {
            let mut j = i;
            while j < n && (b[j] == b' ' || b[j] == b'\t') {
                j += 1;
            }
            let next_is_open = j < n && (b[j] == b'<' || b[j] == b'{');
            if prev_is_close && next_is_open {
                out.push(' ');
            } else {
                out.push_str(&line[i..j]);
            }
            i = j;
            continue;
        }

        // Any other char (text, placeholder) → content, breaks the close state.
        let l = utf8_len(c);
        out.push_str(&line[i..i + l]);
        prev_is_close = false;
        i += l;
    }
    out
}

/// If a continuation line closes its pending tag, return `Some(self_closing)`.
/// Scans for the first unquoted `>`.
fn tag_closes_here(line: &str) -> Option<bool> {
    let b = line.as_bytes();
    let n = b.len();
    let mut i = 0;
    while i < n {
        match b[i] {
            b'>' => return Some(i > 0 && b[i - 1] == b'/'),
            b'"' | b'\'' => {
                let q = b[i];
                i += 1;
                while i < n && b[i] != q {
                    i += 1;
                }
            }
            _ => {}
        }
        i += 1;
    }
    None
}

/// End index (exclusive) of an HTML tag starting at `<`, skipping quoted attrs.
fn tag_end(s: &str, start: usize) -> usize {
    let b = s.as_bytes();
    let n = b.len();
    let mut i = start + 1;
    while i < n {
        match b[i] {
            b'>' => return i + 1,
            b'"' | b'\'' => {
                let q = b[i];
                i += 1;
                while i < n && b[i] != q {
                    i += 1;
                }
                i += 1;
            }
            _ => i += 1,
        }
    }
    n
}

/// End index of a `{ … }` directive, skipping quoted strings.
fn brace_end(s: &str, start: usize) -> usize {
    let b = s.as_bytes();
    let n = b.len();
    let mut depth = 0i32;
    let mut i = start;
    while i < n {
        match b[i] {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return i + 1;
                }
            }
            b'"' | b'\'' => {
                let q = b[i];
                i += 1;
                while i < n && b[i] != q {
                    i += 1;
                }
            }
            _ => {}
        }
        i += 1;
    }
    n
}

fn tag_name(tag: &str) -> String {
    tag[1..]
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '-')
        .collect::<String>()
        .to_ascii_lowercase()
}

/// The keyword of a `{#keyword …}` directive.
fn ree_keyword(s: &str) -> String {
    s[2..]
        .chars()
        .take_while(|c| c.is_ascii_alphabetic())
        .collect()
}

// ── Shared block scanners (from increment 1) ─────────────────────────────

fn dedent(s: &str) -> String {
    let min = s
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| l.chars().take_while(|c| *c == ' ' || *c == '\t').count())
        .min()
        .unwrap_or(0);
    s.lines()
        .map(|l| {
            let cut: usize = l
                .chars()
                .take(min)
                .take_while(|c| *c == ' ' || *c == '\t')
                .map(|c| c.len_utf8())
                .sum();
            &l[cut..]
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn author_level(ws: &str, indent: &str) -> usize {
    if indent.starts_with('\t') || indent.is_empty() {
        ws.chars().take_while(|c| *c == '\t').count()
    } else {
        ws.chars().take_while(|c| *c == ' ').count() / indent.len().max(1)
    }
}

fn find_rawjs_end(src: &str, open: usize) -> Option<(usize, usize, usize)> {
    let inner_start = open + 2;
    let rest = &src[inner_start..];
    let rb = rest.as_bytes();
    let mut depth: i32 = 0;
    for t in tokenize(rest) {
        match t.kind {
            TokKind::Open if rb[t.start] == b'{' => depth += 1,
            TokKind::Close if rb[t.start] == b'}' => {
                if depth == 0 {
                    let close = inner_start + t.start;
                    if src.as_bytes().get(close + 1) == Some(&b'}') {
                        return Some((inner_start, close, close + 2));
                    }
                    return None;
                }
                depth -= 1;
            }
            _ => {}
        }
    }
    None
}

/// Whether a `<script …>` opening tag is JavaScript (so its body may be
/// formatted). No `type`, or a JS type → yes; any other type (json, module maps,
/// templates, wasm) → no.
fn script_is_js(open_tag: &str) -> bool {
    let lower = open_tag.to_ascii_lowercase();
    let ty = match lower.find("type").and_then(|p| lower[p + 4..].find('=').map(|q| p + 4 + q + 1)) {
        Some(start) => start,
        None => return true, // no type attribute → classic JS
    };
    let after = lower[ty..].trim_start();
    let val: String = if let Some(stripped) = after.strip_prefix(['"', '\'']) {
        stripped.chars().take_while(|c| *c != '"' && *c != '\'').collect()
    } else {
        after.chars().take_while(|c| !c.is_whitespace() && *c != '>').collect()
    };
    matches!(
        val.trim(),
        "" | "module"
            | "text/javascript"
            | "application/javascript"
            | "text/ecmascript"
            | "application/ecmascript"
    )
}

/// True when every byte since the start of the line (or the start of `src`)
/// is a space or tab — i.e. `pos` is a fence marker's only possible column.
fn at_line_start(src: &str, pos: usize) -> bool {
    let b = src.as_bytes();
    let mut j = pos;
    while j > 0 {
        j -= 1;
        match b[j] {
            b'\n' => return true,
            b' ' | b'\t' => continue,
            _ => return false,
        }
    }
    true
}

/// Length of the run of `mark` characters starting at `pos`, if it is 3 or
/// more (the minimum Markdown fence length) — regardless of what precedes it
/// on the line. Used both for a clean own-line fence opener and for a fence
/// glued to preceding content (`<md-text type="code">```ts`), which is a
/// valid opener in this project's own convention even though it isn't at true
/// line start.
fn fence_run_len_here(src: &str, pos: usize) -> Option<usize> {
    let b = src.as_bytes();
    let n = b.len();
    let mark = b[pos];
    if mark != b'`' && mark != b'~' {
        return None;
    }
    let mut j = pos;
    while j < n && b[j] == mark {
        j += 1;
    }
    let run_len = j - pos;
    if run_len < 3 { None } else { Some(run_len) }
}

/// True when the line containing `pos` is, once trimmed, exactly a run of
/// `mark` of length `>= run_len` and nothing else — a valid closing fence line
/// for an opener of that mark and length.
fn closer_run_here(src: &str, pos: usize, mark: u8, run_len: usize) -> bool {
    let b = src.as_bytes();
    let n = b.len();
    let mut line_start = pos;
    while line_start > 0 && b[line_start - 1] != b'\n' {
        line_start -= 1;
    }
    let mut line_end = pos;
    while line_end < n && b[line_end] != b'\n' {
        line_end += 1;
    }
    let line = src[line_start..line_end].trim();
    !line.is_empty() && line.len() >= run_len && line.bytes().all(|c| c == mark)
}

/// If a Markdown fenced code block (` ``` `/`~~~`, 3+ marks, opening the line at
/// `pos`) starts here, return the end of its closing fence line (or end of file
/// if unterminated). Content between the fences is prose/code-sample text —
/// often embedded in a component slot (`<md-text type="code"> … </md-text>`) —
/// not real markup, so a bare `<word>` inside it must never be read as an HTML
/// tag by the depth-based indenter. Masked as one opaque `Verbatim` block, the
/// same way `<pre>`/`<textarea>` already are.
///
/// Only fires when `pos` is at true line start; a fence glued to preceding
/// content on the same line is handled separately (see `glued_fence` in
/// `extract_blocks`) because masking it here would also swallow that
/// preceding content into the verbatim span.
fn fence_here(src: &str, pos: usize) -> Option<usize> {
    if !at_line_start(src, pos) {
        return None;
    }
    let b = src.as_bytes();
    let n = b.len();
    let mark = b[pos];
    let run_len = fence_run_len_here(src, pos)?;
    let j = pos + run_len;
    // A backtick fence's info string can't itself contain a backtick
    // (CommonMark); skip to end of the opener's line either way.
    let mut k = j;
    while k < n && b[k] != b'\n' {
        k += 1;
    }
    let mut line_start = if k < n { k + 1 } else { k };
    loop {
        if line_start >= n {
            return Some(n); // unterminated: rest of file is fence content
        }
        let mut p = line_start;
        while p < n && b[p] != b'\n' {
            p += 1;
        }
        let line = src[line_start..p].trim();
        let is_closer = !line.is_empty()
            && line.chars().all(|c| c == mark as char)
            && line.len() >= run_len;
        if is_closer {
            // Stop at the closer, not past its newline — the newline is
            // ordinary markup structure (like the boundary after `</pre>`)
            // and must stay outside the masked span so the line following
            // the fence isn't spliced onto its last line.
            return Some(p);
        }
        line_start = if p < n { p + 1 } else { p };
    }
}

fn open_tag_here(src: &str, pos: usize) -> Option<(&'static str, Lang)> {
    for (tag, lang) in [("script", Lang::Js), ("style", Lang::Css)] {
        if tag_matches(src, pos, tag) {
            return Some((tag, lang));
        }
    }
    None
}

fn verbatim_tag_here(src: &str, pos: usize) -> Option<&'static str> {
    for tag in ["pre", "textarea"] {
        if tag_matches(src, pos, tag) {
            return Some(tag);
        }
    }
    None
}

fn tag_matches(src: &str, pos: usize, tag: &str) -> bool {
    let rest = &src[pos..];
    rest.len() > tag.len() + 1
        && rest.as_bytes()[0] == b'<'
        && rest[1..].to_ascii_lowercase().starts_with(tag)
        && matches!(rest.as_bytes()[1 + tag.len()], b'>' | b' ' | b'\t' | b'/' | b'\n' | b'\r')
}

fn element_content(src: &str, pos: usize, tag: &str) -> Option<(usize, usize, usize)> {
    let open_gt = pos + src[pos..].find('>')?;
    if src.as_bytes()[open_gt - 1] == b'/' {
        return None;
    }
    let content_start = open_gt + 1;
    let close_marker = format!("</{}", tag);
    let rel = find_outside_literals(&src[content_start..], &close_marker)?;
    let content_end = content_start + rel;
    let after = content_end + src[content_end..].find('>').map(|p| p + 1)?;
    Some((content_start, content_end, after))
}

fn find_outside_literals(hay: &str, needle: &str) -> Option<usize> {
    let lower = hay.to_ascii_lowercase();
    let mut from = 0;
    while let Some(rel) = lower[from..].find(needle) {
        let pos = from + rel;
        if !inside_literal(hay, pos) {
            return Some(pos);
        }
        from = pos + 1;
    }
    None
}

fn inside_literal(hay: &str, pos: usize) -> bool {
    for t in tokenize(hay) {
        if matches!(
            t.kind,
            TokKind::Str | TokKind::Template | TokKind::Regex | TokKind::LineComment | TokKind::BlockComment
        ) && pos >= t.start
            && pos < t.end
        {
            return true;
        }
    }
    false
}

fn utf8_len(b: u8) -> usize {
    match b {
        0x00..=0x7f => 1,
        0xc0..=0xdf => 2,
        0xe0..=0xef => 3,
        _ => 4,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fmt(s: &str) -> String {
        format_ree(s, "\t")
    }

    #[test]
    fn indents_nested_tags() {
        let input = "<div>\n<p>hi</p>\n</div>\n";
        assert_eq!(fmt(input), "<div>\n\t<p>hi</p>\n</div>\n");
    }

    #[test]
    fn fixes_under_indentation() {
        let input = "<div>\n<span>\n<b>x</b>\n</span>\n</div>\n";
        assert_eq!(fmt(input), "<div>\n\t<span>\n\t\t<b>x</b>\n\t</span>\n</div>\n");
    }

    #[test]
    fn void_and_self_closing_do_not_indent() {
        let input = "<div>\n<input type=\"text\" />\n<br>\n<img src=\"x\">\n</div>\n";
        assert_eq!(fmt(input), "<div>\n\t<input type=\"text\" />\n\t<br>\n\t<img src=\"x\">\n</div>\n");
    }

    #[test]
    fn ree_block_directives_indent() {
        let input = "{#if show}\n<div>x</div>\n{/if}\n";
        assert_eq!(fmt(input), "{#if show}\n\t<div>x</div>\n{/if}\n");
    }

    #[test]
    fn ree_else_dedents() {
        let input = "{#if a}\n<p>1</p>\n{:else}\n<p>2</p>\n{/if}\n";
        assert_eq!(fmt(input), "{#if a}\n\t<p>1</p>\n{:else}\n\t<p>2</p>\n{/if}\n");
    }

    #[test]
    fn script_indented_and_content_reindented() {
        let input = "<div>\n<script>\nif (a) {\nb()\n}\n</script>\n</div>\n";
        let out = fmt(input);
        assert!(out.contains("\t<script>\n\t\tif (a) {\n\t\t\tb()\n\t\t}\n\t</script>"), "got:\n{out}");
    }

    #[test]
    fn pre_is_verbatim() {
        let input = "<div>\n<pre>\n  weird\n    indent\n</pre>\n</div>\n";
        let out = fmt(input);
        assert!(out.contains("  weird\n    indent"), "pre mangled:\n{out}");
    }

    // Regression: a fenced code sample inside a custom element (`<md-text
    // type="code">`, common in the docs site's own .ree templates) is not a
    // <pre>/<textarea>, so its content used to be scanned as ordinary markup.
    // A bare `<word>`-shaped line inside the fence — e.g. a placeholder like
    // `<blog_dir>` in a shell comment — was then read as an unclosed HTML tag
    // open, which threw off nesting depth for every line after it: the closing
    // fence and the tag that follows both drifted one level deeper, and on a
    // second format pass the drift compounded further (not idempotent).
    #[test]
    fn fenced_code_block_is_verbatim() {
        let input = "<md-text type=\"code\">\n\t```bash\n\tbun rss  # dist/<blog_dir>/feed.xml\n\t```\n</md-text>\n\n<div>after</div>\n";
        assert_eq!(fmt(input), input, "fence content must not be touched");
        assert_eq!(fmt(&fmt(input)), input, "must be idempotent");
    }

    #[test]
    fn fenced_code_block_bare_tag_does_not_shift_depth() {
        // Same shape without the wrapping component, and with a plain tag
        // rather than an underscore-name one, to isolate the mechanism from
        // any md-text-specific handling.
        let input = "<div>\n\t```html\n\techo <foo>\n\t```\n</div>\n<p>sibling</p>\n";
        assert_eq!(fmt(input), input);
    }

    #[test]
    fn tilde_fence_is_verbatim() {
        let input = "<div>\n\t~~~html\n\t<foo>\n\t~~~\n</div>\n<p>sibling</p>\n";
        assert_eq!(fmt(input), input);
    }

    #[test]
    fn fence_inside_ree_if_block() {
        // The wrapping tag and directive still get normal Rule-2 indentation;
        // only the fence's *own* content stays opaque to tag scanning and
        // untouched (the `const x = <foo>;` line keeps its author indent).
        let input = "{#if show}\n<md-text type=\"code\">\n\t```js\n\tconst x = <foo>;\n\t```\n</md-text>\n{/if}\n<p>after</p>\n";
        let expected = "{#if show}\n\t<md-text type=\"code\">\n\t\t```js\n\tconst x = <foo>;\n\t```\n\t</md-text>\n{/if}\n<p>after</p>\n";
        let out = fmt(input);
        assert_eq!(out, expected, "got:\n{out}");
        assert_eq!(fmt(&out), out, "must be idempotent");
    }

    #[test]
    fn unterminated_fence_does_not_panic() {
        // No closing fence: masked to end-of-file rather than mis-scanned.
        let input = "<div>\n\t```js\n\tconst x = <foo>;\n</div>\n";
        let out = fmt(input);
        assert_eq!(out, input, "unterminated fence must be left alone, not reformatted");
    }

    // Regression: a fence that opens glued to preceding content on its line
    // (`<md-text type="code">```ts`, no newline before the backticks — this
    // project's own docs use exactly this shape to save a line) is correctly
    // left unmasked, since masking it would swallow the tag into the verbatim
    // span. But its bare, own-line closer (```) satisfies `at_line_start` on
    // its own and — without tracking the glued opener — used to be misread as
    // the start of a *new* fence, which then greedily consumed everything up
    // to the next accidental closer: real content downstream (another
    // md-text/fence block, its wrapping tags, sibling elements) got corrupted
    // or re-indented. This is the actual failure mode found in
    // docs/reeweb/index.ree in production.
    #[test]
    fn glued_fence_opener_does_not_corrupt_later_content() {
        let input = concat!(
            "<div>\n",
            "\t<md-text type=\"code\">```ts\n",
            "\t\tconst x = 1\n",
            "\t\t```\n",
            "\t</md-text>\n",
            "</div>\n",
            "\n",
            "<md-text type=\"code\">\n",
            "\t```bash\n",
            "\tbun rss  # dist/<blog_dir>/feed.xml\n",
            "\t```\n",
            "</md-text>\n",
            "\n",
            "<p>sibling</p>\n",
        );
        let out = fmt(input);
        assert_eq!(out, input, "got:\n{out}");
        assert_eq!(fmt(&out), out, "must be idempotent");
    }

    #[test]
    fn glued_fence_opener_alone_is_left_untouched() {
        let input = "<div>\n\t<md-text type=\"code\">```ts\n\t\tconst x = 1\n\t\t```\n\t</md-text>\n</div>\n<p>after</p>\n";
        assert_eq!(fmt(input), input);
    }

    #[test]
    fn single_backtick_inline_code_still_scanned_normally() {
        // A single backtick is inline code, not a fence opener (needs 3+); the
        // surrounding tag's depth tracking must be unaffected.
        let input = "<div>\n<p>`x` <foo> stays inline text</p>\n</div>\n";
        let out = fmt(input);
        assert!(out.contains("\t<p>`x` <foo> stays inline text</p>"), "got:\n{out}");
    }

    #[test]
    fn already_formatted_is_stable() {
        let input = "<field-wrapper class=\"grid\">\n\t<label>x</label>\n\t<input type=\"text\" />\n</field-wrapper>\n";
        assert_eq!(fmt(input), input);
        assert_eq!(fmt(&fmt(input)), fmt(input));
    }

    #[test]
    fn inline_expressions_untouched() {
        let input = "<h1 class=\"{= _class}\">{= title}</h1>\n";
        assert_eq!(fmt(input), input);
    }

    #[test]
    fn unescaped_html_directive_untouched() {
        let input = "<div>{- raw_html}</div>\n";
        assert_eq!(fmt(input), input);
    }

    #[test]
    fn trimmed_text_directive_untouched() {
        let input = "<div>{_ label}</div>\n";
        assert_eq!(fmt(input), input);
    }

    #[test]
    fn multi_line_tag_attributes_indent() {
        let input = "<div>\n<a\nhref=\"/x\"\nclass=\"btn\"\n>\nlink\n</a>\n</div>\n";
        let out = fmt(input);
        assert_eq!(
            out,
            "<div>\n\t<a\n\t\thref=\"/x\"\n\t\tclass=\"btn\"\n\t>\n\t\tlink\n\t</a>\n</div>\n",
            "got:\n{out}"
        );
        assert_eq!(fmt(&out), out, "not idempotent");
    }

    #[test]
    fn json_script_is_verbatim() {
        // A non-JS script type must not be reformatted (no trailing commas → no
        // JSON breakage).
        let input = "<script type=\"application/json\">\n{\n\t\"a\": 1,\n\t\"b\": [\n\t\t2\n\t]\n}\n</script>\n";
        assert_eq!(fmt(input), input);
    }

    #[test]
    fn module_script_is_formatted() {
        let input = "<script type=\"module\">\nif (a) {\nb()\n}\n</script>\n";
        let out = fmt(input);
        assert!(out.contains("if (a) {\n\t\tb()\n\t}"), "module not formatted:\n{out}");
    }

    #[test]
    fn ree_directives_in_script_are_verbatim() {
        // {#each}/{~ } inside a script must not be mangled into { #each }/{ ~ }.
        let input = "<script>\n{#each xs as x}\ndo({~ x});\n{/each}\n</script>\n";
        assert_eq!(fmt(input), input);
    }

    #[test]
    fn unescaped_and_trimmed_directives_in_script_are_verbatim() {
        // {- }/{_ } inside a script must not be mangled by JS formatting.
        let input = "<script>\n{#each xs as x}\ndo({- x});\ndo({_ x});\n{/each}\n</script>\n";
        assert_eq!(fmt(input), input);
    }

    #[test]
    fn same_line_children_preserved() {
        // Rule 1: never split; a line with open+close stays one line.
        let input = "<ul>\n<li>a</li><li>b</li>\n</ul>\n";
        assert_eq!(fmt(input), "<ul>\n\t<li>a</li><li>b</li>\n</ul>\n");
    }

    #[test]
    fn trailing_each_closer_splits_and_dedents() {
        let input = "<ul>\n{#each xs as x}\n<li>{~ md(x)}</li>{/each}\n</ul>\n";
        let expected = "<ul>\n\t{#each xs as x}\n\t\t<li>{~ md(x)}</li>\n\t{/each}\n</ul>\n";
        assert_eq!(fmt(input), expected);
    }

    #[test]
    fn trailing_closer_is_idempotent() {
        let once = fmt("<ul>\n{#each xs as x}\n<li>a</li>{/each}\n</ul>\n");
        assert_eq!(fmt(&once), once);
    }

    #[test]
    fn sibling_elements_not_split() {
        // Both closers matched by same-line openers — stays one line.
        let input = "<ul>\n<li>a</li><li>b</li>\n</ul>\n";
        assert_eq!(fmt(input), "<ul>\n\t<li>a</li><li>b</li>\n</ul>\n");
    }

    #[test]
    fn balanced_same_line_group_not_split() {
        // Every closer matched on the line → nothing structural to peel.
        let input = "<div>\n<div class=\"m\"><h2>x</h2><p>y</p></div>\n</div>\n";
        assert_eq!(fmt(input), "<div>\n\t<div class=\"m\"><h2>x</h2><p>y</p></div>\n</div>\n");
    }

    #[test]
    fn stacked_trailing_closers_staircase() {
        let input = "<section>\n{#each xs as x}\n{#if x}\n<li>a</li>{/if}{/each}\n</section>\n";
        let expected = "<section>\n\t{#each xs as x}\n\t\t{#if x}\n\t\t\t<li>a</li>\n\t\t{/if}\n\t{/each}\n</section>\n";
        assert_eq!(fmt(input), expected);
    }

    #[test]
    fn trailing_html_block_closer_splits() {
        let input = "<section>\n<div>\n<span>x</span></div>\n</section>\n";
        let expected = "<section>\n\t<div>\n\t\t<span>x</span>\n\t</div>\n</section>\n";
        assert_eq!(fmt(input), expected);
    }

    #[test]
    fn lone_closer_unchanged() {
        // No content before the closer → existing leading-closer path handles it.
        let input = "{#if a}\n<p>1</p>\n{/if}\n";
        assert_eq!(fmt(input), "{#if a}\n\t<p>1</p>\n{/if}\n");
    }

    #[test]
    fn inter_tag_whitespace_collapses() {
        // Balanced block on one line stays one line (Rule 1), but the inter-tag
        // tab/space runs collapse to a single space (Tier-1 trim & space).
        let input = "<ul>\n{#each xs as x}\t\t\t<li>a</li>\t\t\t{/each}\n</ul>\n";
        let expected = "<ul>\n\t{#each xs as x} <li>a</li> {/each}\n</ul>\n";
        assert_eq!(fmt(input), expected);
    }

    #[test]
    fn inter_tag_collapse_is_idempotent() {
        let once = fmt("<ul>\n{#each xs as x}\t\t<li>a</li>\t\t{/each}\n</ul>\n");
        assert_eq!(fmt(&once), once);
    }

    #[test]
    fn text_whitespace_preserved() {
        // Whitespace inside text content is not inter-tag → untouched.
        let input = "<p>hello   world</p>\n";
        assert_eq!(fmt(input), input);
    }

    #[test]
    fn whitespace_inside_attribute_preserved() {
        // A `>`/whitespace/`<` sequence inside a quoted attribute must not be
        // treated as inter-tag.
        let input = "<a title=\"a >   < b\">x</a>\n";
        assert_eq!(fmt(input), input);
    }

    #[test]
    fn over_indented_lines_pulled_back() {
        // Rule 2: structural depth wins in both directions — over-indented lines
        // are corrected, not preserved.
        let input = "{#each xs as x}\n\t\t\t\t\t<li>a</li>\n\t\t\t\t\t\t\t\t{/each}\n";
        let expected = "{#each xs as x}\n\t<li>a</li>\n{/each}\n";
        assert_eq!(fmt(input), expected);
    }

    #[test]
    fn trailing_whitespace_trimmed() {
        // Rule 3: trailing whitespace is stripped.
        let input = "<div>   \n\t<p>x</p>\t\n</div>\n";
        assert_eq!(fmt(input), "<div>\n\t<p>x</p>\n</div>\n");
    }
}
