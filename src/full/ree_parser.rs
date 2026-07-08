//! Custom AST-based parser and printer for .ree template files.
//!
//! Parses `.ree` files (HTML + Ree template syntax) into an AST and prints
//! them with consistent tab indentation, proper nesting, and smart line folding.

// ═══════════════════════════════════════════════════════════════
// AST Types
// ═══════════════════════════════════════════════════════════════

#[derive(Debug, Clone)]
pub(crate) enum Node {
    Text(String),
    Element {
        tag: String,
        attrs: Vec<String>,
        children: Vec<Node>,
        self_closing: bool,
        /// True if the source had opening and closing tags on the same line.
        /// The printer preserves this: inline elements stay on one line,
        /// block elements get their children indented.
        inline: bool,
        /// True when the source had a bare opening tag with no matching close
        /// and no children (e.g. `<nav data-x{~ expr }>` on its own). The
        /// printer emits just the opening tag, never a synthesized `</tag>`.
        unclosed: bool,
    },
    ReeBlock {
        keyword: String,
        expr: String,
        children: Vec<Node>,
        else_children: Option<Vec<Node>>,
        /// True if the source had {#keyword} and {/keyword} on the same line.
        inline: bool,
    },
    ReeExpr(String),
    ReeCall(String),
    /// `{_ expr}` — trimmed text.
    ReeTrim(String),
    /// `{- expr}` — unescaped text.
    ReeUnescaped(String),
    ReeDirective(String),
    Comment(String),
    RawJs(String),
}

// ═══════════════════════════════════════════════════════════════
// Public API
// ═══════════════════════════════════════════════════════════════

pub(crate) fn format_ree(input: &str, wrap_width: usize, oneline: usize) -> String {
    let normalized = input.replace("\r\n", "\n");
    let nodes = parse(&normalized);
    let out = print_nodes(&nodes, wrap_width, oneline);
    // Normalize to exactly one trailing newline
    let trimmed = out.trim_end_matches('\n');
    format!("{}\n", trimmed)
}

// ═══════════════════════════════════════════════════════════════
// Parser
// ═══════════════════════════════════════════════════════════════

fn parse(input: &str) -> Vec<Node> {
    let (nodes, _) = parse_nodes(input, Stop::None);
    nodes
}

enum Stop {
    None,
    /// (tag_name, optional_ree_keyword) — when nested inside a Ree block,
    /// also stop at {:else} and {/keyword} so they aren't consumed as
    /// children of the element.
    CloseTag(String, Option<String>),
    ReeClose(String),
}

/// Extract the Ree block keyword from a Stop condition, if any.
/// This is threaded through to element parsers so they also stop at
/// {:else} and {/keyword} markers when nested inside a Ree block.
fn ree_keyword_from_stop(stop: &Stop) -> Option<String> {
    match stop {
        Stop::ReeClose(kw) => Some(kw.clone()),
        Stop::CloseTag(_, kw) => kw.clone(),
        Stop::None => None,
    }
}

fn parse_nodes(input: &str, stop: Stop) -> (Vec<Node>, &str) {
    let ree_keyword = ree_keyword_from_stop(&stop);
    let mut remaining = input;
    let mut nodes = Vec::new();

    while !remaining.is_empty() {
        if check_stop(remaining, &stop) {
            return (nodes, remaining);
        }

        let next = find_next_special(remaining);

        if next > 0 {
            let text = &remaining[..next];
            if !text.is_empty() {
                nodes.push(Node::Text(text.to_string()));
            }
            remaining = &remaining[next..];
        }

        if remaining.is_empty() {
            break;
        }

        if check_stop(remaining, &stop) {
            return (nodes, remaining);
        }

        let (node_opt, after) = parse_token(remaining, &ree_keyword);
        if let Some(node) = node_opt {
            nodes.push(node);
        }
        remaining = after;
    }

    (nodes, remaining)
}

/// Check if a string starts with a Ree else marker: {:else}, {:else }, {:else if ...}
fn is_ree_else_marker(s: &str) -> bool {
    if let Some(rest) = s.strip_prefix("{:else") {
        rest.starts_with('}') || rest.starts_with(" }") || rest.starts_with(" if") || rest.starts_with("\tif")
    } else {
        false
    }
}

/// Check whether `input` starts with a Ree close tag for `keyword`, including
/// variants with trailing whitespace before `}` (e.g. `{/if }`).
fn is_ree_close_tag(input: &str, keyword: &str) -> bool {
    if !input.starts_with("{/") {
        return false;
    }
    let end = find_brace_end(input);
    if end <= 1 {
        return false;
    }
    let inner = input[1..end - 1].trim();
    inner == format!("/{}", keyword) || inner == format!("/ {}", keyword)
}

fn check_stop(remaining: &str, stop: &Stop) -> bool {
    match stop {
        Stop::None => false,
        Stop::CloseTag(tag, ree_keyword) => {
            // Check for closing </tag>
            if let Some(after) = remaining.strip_prefix("</") {
                if let Some(gt) = after.find('>') {
                    let name = after[..gt].trim();
                    if name.eq_ignore_ascii_case(tag) {
                        return true;
                    }
                }
            }
            // Also stop at Ree block markers when nested inside a Ree block.
            // Otherwise {:else} and {/if} get consumed as children of this element.
            if let Some(ref kw) = ree_keyword {
                if is_ree_close_tag(remaining, kw) || is_ree_else_marker(remaining) {
                    return true;
                }
            }
            false
        }
        Stop::ReeClose(keyword) => {
            is_ree_close_tag(remaining, &keyword) || is_ree_else_marker(remaining)
        }
    }
}

fn find_next_special(input: &str) -> usize {
    let mut i = 0;
    let len = input.len();
    let bytes = input.as_bytes();
    while i < len {
        // Skip bytes that are not at char boundaries (inside multi-byte UTF-8)
        if !input.is_char_boundary(i) {
            i += 1;
            continue;
        }
        match bytes[i] {
            b'<' => {
                // Don't skip HTML comments — return the position so parse_token handles them
                return i;
            }
            b'{' => {
                // Don't skip {{ }} — let parse_token handle it as RawJs
                return i;
            }
            _ => {}
        }
        i += 1;
    }
    len
}

fn find_next_special_in_raw(input: &str, close_marker: &str) -> usize {
    let mut i = 0;
    let len = input.len();
    let bytes = input.as_bytes();
    let marker_bytes = close_marker.as_bytes();
    while i < len {
        if !input.is_char_boundary(i) {
            i += 1;
            continue;
        }
        if i + marker_bytes.len() <= len && &bytes[i..i + marker_bytes.len()] == marker_bytes {
            return i;
        }
        match bytes[i] {
            b'{' if i + 1 < len => {
                let next = bytes[i + 1];
                // Inside <script>/<style>, only split at Ree BLOCKS ({#, {/, {:)
                // and {{ (raw JS). Leave {=, {~, {_, {- as raw text — SWC post-processor handles them.
                if next == b'#' || next == b'/' || next == b':' || next == b'{' {
                    return i;
                }
                // It's a bare { or inline Ree expression ({=, {~) — skip past it
                i += 1;
                continue;
            }
            b'{' => {
                i += 1;
                continue;
            }
            b'<' if input[i..].starts_with("<!--") => {
                // Isolate HTML comments inside script/style/pre. Any other `<`
                // is opaque raw content (JS comparisons/generics, `<` in template
                // literals or strings, preformatted markup) and must NOT split the
                // text — doing so fragments the JS and corrupts it before SWC runs.
                return i;
            }
            _ => {}
        }
        i += 1;
    }
    len
}

fn parse_token<'a>(input: &'a str, ree_keyword: &Option<String>) -> (Option<Node>, &'a str) {
    if input.starts_with("<!--") {
        parse_comment(input)
    } else if input.to_ascii_lowercase().starts_with("<!doctype") {
        // DOCTYPE is a special SGML declaration, not an HTML element.
        // Emit it as raw text up to (and including) the closing >.
        if let Some(end) = input.find('>') {
            (Some(Node::Text(input[..end + 1].to_string())), &input[end + 1..])
        } else {
            (Some(Node::Text(input.to_string())), "")
        }
    } else if input.starts_with("</") {
        if let Some(gt) = input.find('>') {
            (None, &input[gt + 1..])
        } else {
            (None, "")
        }
    } else if input.starts_with('<') {
        parse_html_tag(input, ree_keyword)
    } else if input.starts_with("{#if")
        || input.starts_with("{#each")
        || input.starts_with("{#with")
    {
        parse_ree_block_open(input)
    } else if input.starts_with("{#layout") || input.starts_with("{#include") {
        parse_ree_directive(input)
    } else if input.starts_with("{/if") || input.starts_with("{/each") || input.starts_with("{/with") {
        parse_ree_block_close(input)
    } else if input.starts_with("{:else") {
        let end = find_brace_end(input);
        if end > 0 {
            (Some(Node::Text(input[..end].to_string())), &input[end..])
        } else {
            // UTF-8 safe single character fallback
            let ch = input.chars().next().unwrap();
            let next = &input[ch.len_utf8()..];
            (Some(Node::Text(ch.to_string())), next)
        }
    } else if input.starts_with("{=") {
        parse_ree_inline(input, 2, ReeInlineKind::Expr)
    } else if input.starts_with("{~") {
        parse_ree_inline(input, 2, ReeInlineKind::Call)
    } else if input.starts_with("{_") {
        parse_ree_inline(input, 2, ReeInlineKind::Trim)
    } else if input.starts_with("{-") {
        parse_ree_inline(input, 2, ReeInlineKind::Unescaped)
    } else if input.starts_with("{{") {
        parse_ree_raw_js(input)
    } else if let Some(rest) = input.strip_prefix('{') {
        // UTF-8 safe single character fallback for rogue braces
        let ch = input.chars().next().unwrap();
        (Some(Node::Text(ch.to_string())), rest)
    } else {
        // UTF-8 safe character slicing for text consumption
        let mut chars = input.chars();
        if let Some(ch) = chars.next() {
            (Some(Node::Text(ch.to_string())), chars.as_str())
        } else {
            (None, "")
        }
    }
}

/// Find the end of a brace-delimited expression using byte-level iteration.
/// Skips quoted strings to avoid false matches on `}` inside attribute values.
fn find_brace_end(input: &str) -> usize {
    let mut depth: i32 = 0;
    let bytes = input.as_bytes();
    let len = bytes.len();
    let mut i = 0;
    while i < len {
        let b = bytes[i];
        if b == b'{' {
            depth += 1;
        } else if b == b'}' {
            depth -= 1;
            if depth == 0 {
                return i + 1;
            }
        } else if b == b'"' {
            // Skip double-quoted string
            i += 1;
            while i < len && bytes[i] != b'"' {
                if bytes[i] == b'\\' && i + 1 < len {
                    i += 2;
                } else {
                    i += 1;
                }
            }
            // i now points at closing " or len; loop will i+=1 past it
        } else if b == b'\'' {
            // Skip single-quoted string
            i += 1;
            while i < len && bytes[i] != b'\'' {
                if bytes[i] == b'\\' && i + 1 < len {
                    i += 2;
                } else {
                    i += 1;
                }
            }
        }
        i += 1;
    }
    input.len()
}

// ── Comment ──────────────────────────────────────────────────

fn parse_comment(input: &str) -> (Option<Node>, &str) {
    if let Some(end) = input[4..].find("-->") {
        let full = end + 4 + 3;
        (Some(Node::Comment(input[..full].to_string())), &input[full..])
    } else {
        (Some(Node::Comment(input.to_string())), "")
    }
}

// ── HTML Element ─────────────────────────────────────────────

fn parse_html_tag<'a>(input: &'a str, ree_keyword: &Option<String>) -> (Option<Node>, &'a str) {
    let after_lt = &input[1..];

    let name_end = after_lt
        .find(|c: char| c.is_whitespace() || c == '>' || c == '/' || c == '{')
        .unwrap_or(after_lt.len());
    let tag = after_lt[..name_end].to_string();
    let remaining = &after_lt[name_end..];

    let (attrs, remaining) = parse_attrs(remaining);

    if let Some(after) = remaining.strip_prefix("/>") {
        return (
            Some(Node::Element { tag, attrs, children: vec![], self_closing: true, inline: true, unclosed: false }),
            after,
        );
    }

    if let Some(after_gt) = remaining.strip_prefix('>') {

        // Void elements (input, br, hr, etc.) don't have children — treat as self-closing
        if is_void_element(&tag) {
            return (
                Some(Node::Element { tag, attrs, children: vec![], self_closing: true, inline: true, unclosed: false }),
                after_gt,
            );
        }

        if tag.eq_ignore_ascii_case("script") || tag.eq_ignore_ascii_case("style") || tag.eq_ignore_ascii_case("pre") {
            let close = format!("</{}", tag.to_lowercase());
            let (children, rem) = parse_raw_block_content(after_gt, &close);
            if rem.starts_with(&close) {
                if let Some(gt) = rem.find('>') {
                    return (
                        Some(Node::Element { tag, attrs, children, self_closing: false, inline: false, unclosed: false }),
                        &rem[gt + 1..],
                    );
                }
            }
            return (
                Some(Node::Element { tag, attrs, children, self_closing: false, inline: false, unclosed: false }),
                rem,
            );
        }

        // Detect inline: closing tag appears before the first newline.
        let close_tag_lower = format!("</{}", tag.to_lowercase());
        let newline_pos = after_gt.find('\n').unwrap_or(after_gt.len());
        let close_pos = after_gt.to_ascii_lowercase().find(&close_tag_lower).unwrap_or(after_gt.len());
        let inline = close_pos < newline_pos;

        // Thread the Ree keyword into the element's CloseTag stop so that
        // {:else} and {/keyword} markers stop the inner parse_nodes.
        let stop = Stop::CloseTag(tag.clone(), ree_keyword.clone());
        let (children, rem) = parse_nodes(after_gt, stop);
        if rem.starts_with("</") {
            if let Some(gt) = rem.find('>') {
                return (
                    Some(Node::Element { tag, attrs, children, self_closing: false, inline, unclosed: false }),
                    &rem[gt + 1..],
                );
            }
        }
        // No matching close tag was found. If nothing was consumed as children,
        // the source was a bare opening tag - preserve it verbatim (unclosed)
        // rather than synthesizing a `</tag>`. If children were consumed, keep
        // the historical behavior (explicit close in the printer).
        let unclosed = children.is_empty();
        return (
            Some(Node::Element { tag, attrs, children, self_closing: false, inline, unclosed }),
            rem,
        );
    }

    (Some(Node::Element { tag, attrs, children: vec![], self_closing: false, inline: false, unclosed: true }), remaining)
}

fn parse_attrs(input: &str) -> (Vec<String>, &str) {
    let mut attrs = Vec::new();
    let mut remaining = input;
    loop {
        let trimmed = remaining.trim_start();
        let ws = remaining.len() - trimmed.len();
        remaining = &remaining[ws..];
        if remaining.is_empty() || remaining.starts_with('>') || remaining.starts_with("/>") {
            break;
        }
        if let Some((attr, after)) = parse_one_attr(remaining) {
            attrs.push(attr);
            remaining = after;
        } else {
            break;
        }
    }
    (attrs, remaining)
}

fn parse_one_attr(input: &str) -> Option<(String, &str)> {
    // Detect Ree constructs ({#if, {#each, {#with, {=, {~, {_, {-, {/if, {/each, {/with, {:else})
    // inside HTML attribute position and handle them as a single token.
    // Without this, {#if condition}selected{/if} inside <option> would be parsed as
    // separate broken attributes ({#if, m.code===, record.module_code, }selected{/if}).
    if input.starts_with('{') {
        let end = find_brace_end(input);
        if end > 0 {
            return Some((input[..end].to_string(), &input[end..]));
        }
    }

    let name_end = input
        .find(|c: char| c == '=' || c == '>' || c == '{' || c.is_whitespace())
        .unwrap_or(input.len());
    if name_end == 0 {
        return None;
    }
    let name = &input[..name_end];
    let remaining = &input[name_end..];

    // Handle boolean attrs followed by a ree expression, e.g. `data-foo{~ expr }`.
    // Consume the brace expression and return the whole thing as one attr token.
    if remaining.starts_with('{') {
        let brace_end = find_brace_end(remaining);
        let attr = format!("{}{}", name, &remaining[..brace_end]);
        return Some((attr, &remaining[brace_end..]));
    }

    if let Some(after_eq) = remaining.strip_prefix('=') {
        if after_eq.starts_with('"') {
            let mut j = 1;
            while j < after_eq.len() {
                if after_eq.as_bytes()[j] == b'"'
                    && after_eq.as_bytes().get(j.wrapping_sub(1)) != Some(&b'\\')
                {
                    return Some((
                        format!("{}={}", name, &after_eq[..j + 1]),
                        &after_eq[j + 1..],
                    ));
                }
                j += 1;
            }
            return Some((format!("{}={}", name, after_eq), ""));
        } else if after_eq.starts_with('\'') {
            let mut j = 1;
            while j < after_eq.len() {
                if after_eq.as_bytes()[j] == b'\''
                    && after_eq.as_bytes().get(j.wrapping_sub(1)) != Some(&b'\\')
                {
                    return Some((
                        format!("{}={}", name, &after_eq[..j + 1]),
                        &after_eq[j + 1..],
                    ));
                }
                j += 1;
            }
            return Some((format!("{}={}", name, after_eq), ""));
        } else {
            let val_end = after_eq
                .find(|c: char| c.is_whitespace() || c == '>')
                .unwrap_or(after_eq.len());
            return Some((
                format!("{}={}", name, &after_eq[..val_end]),
                &after_eq[val_end..],
            ));
        }
    }

    Some((name.to_string(), remaining))
}

// ── Ree Block ────────────────────────────────────────────────

fn parse_ree_block_open(input: &str) -> (Option<Node>, &str) {
    let end = find_brace_end(input);
    if end == 0 {
        return (Some(Node::Text(input[..1].to_string())), &input[1..]);
    }

    let directive = input[1..end - 1].trim();
    let directive_stripped = directive.strip_prefix('#').unwrap_or(directive);
    let (keyword, expr) = match directive_stripped.find(' ') {
        Some(pos) => (
            directive_stripped[..pos].to_string(),
            directive_stripped[pos + 1..].trim().to_string(),
        ),
        None => (directive_stripped.to_string(), String::new()),
    };
    let remaining = &input[end..];

    // Detect inline: {/keyword} appears before the first newline after the opening tag.
    let close_tag = format!("{{/{}}}", keyword);
    let newline_pos = remaining.find('\n').unwrap_or(remaining.len());
    let close_pos = remaining.find(&close_tag).unwrap_or(remaining.len());
    let inline = close_pos < newline_pos;

    let stop = Stop::ReeClose(keyword.clone());
    let (children, remaining) = parse_nodes(remaining, stop);
    let (else_children, remaining) = parse_else_branch(remaining, &keyword);
    let remaining = skip_ree_close(remaining, &keyword);

    (Some(Node::ReeBlock { keyword, expr, children, else_children, inline }), remaining)
}

fn parse_else_branch<'a>(input: &'a str, keyword: &str) -> (Option<Vec<Node>>, &'a str) {
    let trimmed = input.trim_start();
    let offset = input.len() - trimmed.len();

    // Handle {:else if ...} with optional space before }
    if trimmed.starts_with("{:else if ") || trimmed.starts_with("{:else if\t") {
        if let Some(end) = trimmed.find('}') {
            let rem = &input[offset + end + 1..];
            let stop = Stop::ReeClose(keyword.to_string());
            let (children, rem) = parse_nodes(rem, stop);
            return (Some(children), rem);
        }
    // Handle {:else} and {:else } — both are valid else markers
    } else if let Some(rest) = trimmed.strip_prefix("{:else") {
        if rest.starts_with('}') || rest.starts_with(" }") {
            // Find the actual closing brace
            if let Some(end) = trimmed.find('}') {
                let rem = &input[offset + end + 1..];
                let stop = Stop::ReeClose(keyword.to_string());
                let (children, rem) = parse_nodes(rem, stop);
                return (Some(children), rem);
            }
        }
    }

    (None, input)
}

fn skip_ree_close<'a>(input: &'a str, keyword: &str) -> &'a str {
    let trimmed = input.trim_start();
    let offset = input.len() - trimmed.len();
    if is_ree_close_tag(trimmed, keyword) {
        let end = find_brace_end(trimmed);
        &input[offset + end..]
    } else {
        input
    }
}

// ── Ree Block Close ──────────────────────────────────────────

fn parse_ree_block_close(input: &str) -> (Option<Node>, &str) {
    let end = find_brace_end(input);
    if end > 0 {
        (Some(Node::Text(input[..end].to_string())), &input[end..])
    } else {
        // UTF-8 safe fallback for unclosed `{/...`
        let ch = input.chars().next().unwrap();
        let next = &input[ch.len_utf8()..];
        (Some(Node::Text(ch.to_string())), next)
    }
}

// ── Ree Directive ────────────────────────────────────────────

fn parse_ree_directive(input: &str) -> (Option<Node>, &str) {
    let end = find_brace_end(input);
    if end == 0 {
        return (Some(Node::Text(input[..1].to_string())), &input[1..]);
    }
    (Some(Node::ReeDirective(input[..end].to_string())), &input[end..])
}

// ── Raw JS Block ────────────────────────────────────────────

fn parse_ree_raw_js(input: &str) -> (Option<Node>, &str) {
    if let Some(end) = input[2..].find("}}") {
        let content = input[2..end + 2].to_string();
        (Some(Node::RawJs(content)), &input[end + 4..])
    } else {                (Some(Node::Text(input.to_string())), "")
    }
}

// ── Ree Expression / Call ────────────────────────────────────

enum ReeInlineKind {
    Expr,
    Call,
    Trim,
    Unescaped,
}

fn parse_ree_inline(input: &str, open_len: usize, kind: ReeInlineKind) -> (Option<Node>, &str) {
    let after = &input[open_len..];
    if let Some(pos) = after.find('}') {
        // Trim both sides so formatting is consistent regardless of source spacing.
        // The printer always adds a space after {= / {~ / {_ / {- and before }.
        let raw = &after[..pos];
        let expr = raw.trim().to_string();
        let remaining = &after[pos + 1..];
        let node = match kind {
            ReeInlineKind::Expr => Node::ReeExpr(expr),
            ReeInlineKind::Call => Node::ReeCall(expr),
            ReeInlineKind::Trim => Node::ReeTrim(expr),
            ReeInlineKind::Unescaped => Node::ReeUnescaped(expr),
        };
        (Some(node), remaining)
    } else {
        (Some(Node::Text(input.to_string())), "")
    }
}

// ── Script/Style Raw Block Content ───────────────────────────
//
// For <script> and <style> tags, we parse ONLY Ree blocks ({#if}, {#each}, {#with})
// but NOT inline Ree expressions ({=}, {~}, {_}, {-}). The inline expressions are
// preserved as raw Text and handled later by the SWC post-processor (format_script_blocks).
// This prevents the parser from breaking JS code structure.

fn parse_raw_block_content<'a>(input: &'a str, close_marker: &str) -> (Vec<Node>, &'a str) {
    let mut remaining = input;
    let mut nodes = Vec::new();

    while !remaining.is_empty() {
        if remaining.starts_with(close_marker) {
            break;
        }

        let next = find_next_special_in_raw(remaining, close_marker);

        if next > 0 {
            let text = &remaining[..next];
            if !text.is_empty() {
                nodes.push(Node::Text(text.to_string()));
            }
            remaining = &remaining[next..];
        }

        if remaining.is_empty() || remaining.starts_with(close_marker) {
            break;
        }

        // Parse Ree blocks ({#if}/{#each}/{#with}) inside script/style so their
        // structure is preserved; the JS between the tokens is formatted later.
        // Leave {=}, {~}, {_}, {-} as raw Text — they're handled by SWC post-processing.
        if remaining.starts_with("{#if") || remaining.starts_with("{#each") || remaining.starts_with("{#with") {
            let (node, after) = parse_ree_block_open(remaining);
            if let Some(n) = node { nodes.push(n); }
            remaining = after;
        } else if remaining.starts_with("{:else") || remaining.starts_with(            "{{/if}}") || remaining.starts_with("{/each}") || remaining.starts_with("{/with}") {
            // Pass through close/else tokens — they're part of Ree blocks
            let end = find_brace_end(remaining);
            if end > 0 {
                nodes.push(Node::Text(remaining[..end].to_string()));
                remaining = &remaining[end..];
            } else {
                remaining = &remaining[1..];
            }            } else if remaining.starts_with("{{") {
            if let Some(end) = remaining[2..].find("}}") {
                nodes.push(Node::RawJs(remaining[2..end + 2].to_string()));
                remaining = &remaining[end + 4..];
            } else {
                nodes.push(Node::Text(remaining.to_string()));
                remaining = "";
            }
        } else if remaining.starts_with('<') {
            // Pass through standalone HTML tags inside script content?
            // This is unusual — just pass through as text
            if let Some(gt) = remaining.find('>') {
                nodes.push(Node::Text(remaining[..gt + 1].to_string()));
                remaining = &remaining[gt + 1..];
            } else {
                nodes.push(Node::Text(remaining.to_string()));
                remaining = "";
            }
        } else {
            // Skip one character and continue gathering text
            // (This handles {=, {~, and anything else as raw text)
            nodes.push(Node::Text(remaining[..1].to_string()));
            remaining = &remaining[1..];
        }
    }

    (nodes, remaining)
}

// ═══════════════════════════════════════════════════════════════
// Printer
// ═══════════════════════════════════════════════════════════════

fn print_nodes(nodes: &[Node], wrap_width: usize, oneline: usize) -> String {
    let mut out = String::new();
    for node in nodes {
        print_node(node, 0, &mut out, wrap_width, oneline);
    }
    out
}

fn print_node(node: &Node, depth: usize, out: &mut String, wrap_width: usize, oneline: usize) {
    match node {
        Node::Text(text) => {
            let trimmed = text.trim();
            if trimmed.is_empty() {
                // Preserve blank lines (2+ newlines) but skip single newlines
                // since block element formatting already adds newlines.
                // Example: \n\n (2 newlines) -> 1 blank line; \n (1 newline) -> no blank line
                let blank_lines = text.matches('\n').count().saturating_sub(1);
                for _ in 0..blank_lines {
                    out.push('\n');
                }
            } else if trimmed.contains('\n') {
                // Multi-line text: indent every non-empty line
                let lines: Vec<&str> = trimmed.lines().collect();
                for (i, line) in lines.iter().enumerate() {
                    let t = line.trim();
                    if !t.is_empty() {
                        out.push_str(&"\t".repeat(depth));
                        out.push_str(t);
                    }
                    if i < lines.len() - 1 {
                        out.push('\n');
                    }
                }
                // Always terminate with a newline so the next sibling (e.g. a
                // {#if} block following raw JS in a <script>) starts on its own
                // line. Matches the single-line branch below; without it the
                // sibling gets glued onto this text's last line.
                out.push('\n');
            } else {
                out.push_str(&"\t".repeat(depth));
                out.push_str(trimmed);
                out.push('\n');
            }
        }
        Node::Element { tag, attrs, children, self_closing, inline, unclosed } => {
            if *unclosed {
                // Bare opening tag with no matching close in source - emit it
                // verbatim, no synthesized `</tag>`.
                let attr_str = format_attrs_inline(attrs);
                out.push_str(&"\t".repeat(depth));
                out.push_str(&format!("{}>", open_tag_head(tag, &attr_str)));
                out.push('\n');
            } else if *self_closing {
                print_self_closing(tag, attrs, depth, out);
            } else if tag.eq_ignore_ascii_case("pre") {
                // Preserve <pre> content verbatim — whitespace is significant.
                let attr_str = format_attrs_inline(attrs);
                let open = format!("{}>", open_tag_head(tag, &attr_str));
                out.push_str(&"\t".repeat(depth));
                out.push_str(&open);
                for child in children {
                    if let Node::Text(t) = child { out.push_str(t); }
                }
                out.push_str(&format!("</{}>", tag));
                out.push('\n');
            } else if children.is_empty() || all_children_are_whitespace(children) {
                print_empty_element(tag, attrs, depth, out, wrap_width);
            } else if *inline && !is_script_or_style(tag) {
                // Author wrote it on one line — always render inline.
                let inline_str = render_node_inline(node);
                out.push_str(&"\t".repeat(depth));
                out.push_str(&inline_str);
                out.push('\n');
            } else if oneline > 0 && !is_script_or_style(tag) && has_no_child_elements(children) {
                // oneline: only collapse leaf elements (no child tags), and only when they fit.
                let inline_str = render_node_inline(node);
                let full_len = depth + inline_str.len(); // tabs counted as 1 char
                if full_len <= oneline {
                    out.push_str(&"\t".repeat(depth));
                    out.push_str(&inline_str);
                    out.push('\n');
                } else {
                    print_block_element(tag, attrs, children, depth, out, wrap_width, oneline);
                }
            } else {
                print_block_element(tag, attrs, children, depth, out, wrap_width, oneline);
            }
        }
        Node::ReeBlock { keyword, expr, children, else_children, inline } => {
            let open = if expr.is_empty() {
                format!("{{#{}}}", keyword)
            } else {
                format!("{{#{} {}}}", keyword, expr)
            };
            if *inline {
                let content: String = children.iter().map(render_node_inline).collect();
                let mut s = format!("{}{}", open, content.trim());
                if let Some(else_nodes) = else_children {
                    let else_content: String = else_nodes.iter().map(render_node_inline).collect();
                    s.push_str(&format!("{{:else}}{}", else_content.trim()));
                }
                s.push_str(&format!("{{/{}}}", keyword));
                out.push_str(&"\t".repeat(depth));
                out.push_str(&s);
                out.push('\n');
            } else {
                out.push_str(&"\t".repeat(depth));
                out.push_str(&open);
                out.push('\n');
                print_ree_block_children(children, depth + 1, out, wrap_width, oneline);
                if let Some(else_nodes) = else_children {
                    out.push_str(&"\t".repeat(depth));
                    out.push_str("{:else}\n");
                    print_ree_block_children(else_nodes, depth + 1, out, wrap_width, oneline);
                }
                out.push_str(&"\t".repeat(depth));
                out.push_str(&format!("{{/{}}}", keyword));
                out.push('\n');
            }
        }
        Node::ReeExpr(expr) => {
            out.push_str(&"\t".repeat(depth));
            out.push_str(&format!("{{= {}}}", expr));
            out.push('\n');
        }
        Node::ReeCall(expr) => {
            out.push_str(&"\t".repeat(depth));
            out.push_str(&format!("{{~ {}}}", expr));
            out.push('\n');
        }
        Node::ReeTrim(expr) => {
            out.push_str(&"\t".repeat(depth));
            out.push_str(&format!("{{_ {}}}", expr));
            out.push('\n');
        }
        Node::ReeUnescaped(expr) => {
            out.push_str(&"\t".repeat(depth));
            out.push_str(&format!("{{- {}}}", expr));
            out.push('\n');
        }
        Node::ReeDirective(text) => {
            out.push_str(&"\t".repeat(depth));
            out.push_str(text);
            out.push('\n');
        }
        Node::Comment(text) => {
            out.push_str(&"\t".repeat(depth));
            out.push_str(text);
            out.push('\n');
        }
        Node::RawJs(code) => {
            if code.contains('\n') {
                // Multi-line raw JS block
                out.push_str(&"\t".repeat(depth));
                out.push_str("{{");
                out.push('\n');
                for line in code.lines() {
                    let trimmed = line.trim();
                    if !trimmed.is_empty() {
                        out.push_str(&"\t".repeat(depth + 1));
                        out.push_str(line.trim_start());
                        out.push('\n');
                    }
                }
                out.push_str(&"\t".repeat(depth));
                out.push_str("}}");
                out.push('\n');
            } else {
                // Single-line raw JS
                out.push_str(&"\t".repeat(depth));
                out.push_str("{{ ");
                out.push_str(code.trim());
                out.push_str(" }}");
                out.push('\n');
            }
        }
    }
}

/// Render a node as a flat string for use inside inline elements/blocks.
/// Interior whitespace is preserved; callers trim the final result.
fn render_node_inline(node: &Node) -> String {
    match node {
        Node::Text(t) => t.to_string(),
        Node::ReeExpr(e) => format!("{{= {}}}", e),
        Node::ReeCall(e) => format!("{{~ {}}}", e),
        Node::ReeTrim(e) => format!("{{_ {}}}", e),
        Node::ReeUnescaped(e) => format!("{{- {}}}", e),
        Node::Comment(c) => c.clone(),
        Node::ReeDirective(t) => t.clone(),
        Node::Element { tag, attrs, children, self_closing, .. } => {
            let attr_str = format_attrs_inline(attrs);
            if *self_closing {
                format!("{} />", open_tag_head(tag, &attr_str))
            } else {
                let tag_open = format!("{}>", open_tag_head(tag, &attr_str));
                let content: String = children.iter().map(render_node_inline).collect();
                format!("{}{}</{}>", tag_open, content.trim(), tag)
            }
        }
        Node::ReeBlock { keyword, expr, children, else_children, .. } => {
            let open = if expr.is_empty() {
                format!("{{#{}}}", keyword)
            } else {
                format!("{{#{} {}}}", keyword, expr)
            };
            let content: String = children.iter().map(render_node_inline).collect();
            let mut result = format!("{}{}", open, content.trim());
            if let Some(else_nodes) = else_children {
                let else_content: String = else_nodes.iter().map(render_node_inline).collect();
                result.push_str(&format!("{{:else}}{}", else_content.trim()));
            }
            result.push_str(&format!("{{/{}}}", keyword));
            result
        }
        Node::RawJs(_) => String::new(),
    }
}

fn print_block_element(tag: &str, attrs: &[String], children: &[Node], depth: usize, out: &mut String, wrap_width: usize, oneline: usize) {
    let attr_str = format_attrs_inline(attrs);
    let tag_line = format!("{}>", open_tag_head(tag, &attr_str));

    if tag_line.len() <= wrap_width {
        out.push_str(&"\t".repeat(depth));
        out.push_str(&tag_line);
        out.push('\n');
    } else {
        out.push_str(&"\t".repeat(depth));
        out.push('<');
        out.push_str(tag);
        out.push('\n');
        for (i, attr) in attrs.iter().enumerate() {
            out.push_str(&"\t".repeat(depth + 1));
            out.push_str(attr);
            if i == attrs.len() - 1 {
                out.push('>');
            }
            out.push('\n');
        }
    }

    for child in children {
        print_node(child, depth + 1, out, wrap_width, oneline);
    }

    out.push_str(&"\t".repeat(depth));
    out.push_str(&format!("</{}>", tag));
    out.push('\n');
}

fn is_script_or_style(tag: &str) -> bool {
    tag.eq_ignore_ascii_case("script") || tag.eq_ignore_ascii_case("style")
}

fn all_children_are_whitespace(children: &[Node]) -> bool {
    children.iter().all(|c| match c {
        Node::Text(t) => t.trim().is_empty(),
        _ => false,
    })
}

fn has_no_child_elements(children: &[Node]) -> bool {
    children.iter().all(|c| !matches!(c, Node::Element { .. } | Node::ReeBlock { .. }))
}

fn format_attrs_inline(attrs: &[String]) -> String {
    attrs.join(" ")
}

/// Build the opening portion of a tag (`<tag` plus its inline attrs), without
/// the closing `>` or ` />`. A brace expression that abuts the tag name in
/// source, e.g. `<details{~ open ? ' open' : '' }>`, is glued back with no
/// separating space; a normal attribute is space-separated as usual.
fn open_tag_head(tag: &str, attr_str: &str) -> String {
    if attr_str.is_empty() {
        format!("<{}", tag)
    } else if attr_str.starts_with('{') {
        format!("<{}{}", tag, attr_str)
    } else {
        format!("<{} {}", tag, attr_str)
    }
}

/// HTML void elements that cannot have children.
/// Per HTML spec: area, base, br, col, embed, hr, img, input, link, meta,
/// param, source, track, wbr.
fn is_void_element(tag: &str) -> bool {
    matches!(tag.to_ascii_lowercase().as_str(),
        "area" | "base" | "br" | "col" | "embed" | "hr" | "img"
        | "input" | "link" | "meta" | "param" | "source" | "track" | "wbr"
    )
}

fn print_empty_element(tag: &str, attrs: &[String], depth: usize, out: &mut String, wrap_width: usize) {
    let attr_str = format_attrs_inline(attrs);

    // Void HTML elements (input, br, hr, etc.) use self-closing syntax <tag />
    if is_void_element(tag) {
        let self_close = format!("{}/>", open_tag_head(tag, &attr_str));
        out.push_str(&"\t".repeat(depth));
        out.push_str(&self_close);
        out.push('\n');
        return;
    }

    // Non-void empty elements: use explicit close tag <tag></tag>
    if !is_script_or_style(tag) {
        let close_tag = format!("{}></{}>", open_tag_head(tag, &attr_str), tag);
        if close_tag.len() + depth <= wrap_width {
            out.push_str(&"\t".repeat(depth));
            out.push_str(&close_tag);
            out.push('\n');
            return;
        }
    }
    // Fall back to <tag></tag> for long lines or script/style tags
    let open = format!("{}>", open_tag_head(tag, &attr_str));
    let close = format!("</{}>", tag);
    out.push_str(&"\t".repeat(depth));
    out.push_str(&open);
    out.push('\n');
    out.push_str(&"\t".repeat(depth));
    out.push_str(&close);
    out.push('\n');
}

/// Check whether a node is an "inline" type (text, expression, or call)
/// that can be grouped with adjacent inline nodes on the same line.
fn is_inline_node(node: &Node) -> bool {
    matches!(
        node,
        Node::Text(_) | Node::ReeExpr(_) | Node::ReeCall(_) | Node::ReeTrim(_) | Node::ReeUnescaped(_) | Node::Comment(_)
    )
}

/// Check whether a node is a blank-line text node that should act as a
/// separator between inline groups rather than being merged into an inline run.
fn is_blank_line_node(node: &Node) -> bool {
    match node {
        Node::Text(t) => t.trim().is_empty() && t.matches('\n').count() > 1,
        _ => false,
    }
}

/// Print consecutive inline nodes on a single line, collapsing whitespace
/// between them to single spaces. This keeps semantically related expressions
/// (e.g. `{= option}` and `{= selectors.per_page}`) together on one line
/// instead of splitting them across separate lines.
fn print_inline_nodes(nodes: &[Node], depth: usize, out: &mut String, _wrap_width: usize) {
    let mut line = String::new();
    for node in nodes {
        match node {
            Node::Text(t) => {
                let trimmed = t.trim();
                if !trimmed.is_empty() {
                    if !line.is_empty() && !line.ends_with(' ') {
                        line.push(' ');
                    }
                    line.push_str(trimmed);
                    if !line.ends_with(' ') {
                        line.push(' ');
                    }
                }
            }
            Node::ReeExpr(e) => {
                line.push_str(&format!("{{= {}}} ", e));
            }
            Node::ReeCall(c) => {
                line.push_str(&format!("{{~ {}}} ", c));
            }
            Node::ReeTrim(e) => {
                line.push_str(&format!("{{_ {}}} ", e));
            }
            Node::ReeUnescaped(e) => {
                line.push_str(&format!("{{- {}}} ", e));
            }
            Node::Comment(c) => {
                let trimmed = c.trim();
                if !trimmed.is_empty() {
                    line.push_str(trimmed);
                    line.push(' ');
                }
            }
            _ => {}
        }
    }
    let trimmed_line = line.trim();
    if !trimmed_line.is_empty() {
        out.push_str(&"\t".repeat(depth));
        out.push_str(trimmed_line);
        out.push('\n');
    }
}

/// Print children of a ReeBlock, grouping consecutive inline nodes (Text,
/// ReeExpr, ReeCall, ReeTrim, ReeUnescaped, Comment) onto a single line instead of splitting them.
/// Block-level children (Element, ReeBlock, etc.) still print on their own lines.
/// Blank-line text nodes (Text with >= 2 newlines, whitespace only) act as
/// separators between inline groups — they are preserved rather than merged.
fn print_ree_block_children(children: &[Node], depth: usize, out: &mut String, wrap_width: usize, oneline: usize) {
    let mut i = 0;
    while i < children.len() {
        if is_inline_node(&children[i]) {
            // Blank-line text nodes act as separators — emit the blank line and continue
            if is_blank_line_node(&children[i]) {
                if let Node::Text(t) = &children[i] {
                    let blank_lines = t.matches('\n').count().saturating_sub(1);
                    for _ in 0..blank_lines {
                        out.push('\n');
                    }
                }
                i += 1;
                continue;
            }

            let start = i;
            i += 1;
            // Extend the inline group until we hit a non-inline or blank-line node
            while i < children.len() && is_inline_node(&children[i]) && !is_blank_line_node(&children[i]) {
                i += 1;
            }
            print_inline_nodes(&children[start..i], depth, out, wrap_width);
        } else {
            print_node(&children[i], depth, out, wrap_width, oneline);
            i += 1;
        }
    }
}

fn print_self_closing(tag: &str, attrs: &[String], depth: usize, out: &mut String) {
    let attr_str = format_attrs_inline(attrs);
    out.push_str(&"\t".repeat(depth));
    out.push('<');
    out.push_str(tag);
    if !attr_str.is_empty() {
        out.push(' ');
        out.push_str(&attr_str);
    }
    // Self-closing elements use <tag /> syntax
    out.push_str(" />\n");
}

// ═══════════════════════════════════════════════════════════════
// Tests
// ═══════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn simple_element() {
        // Source has both tags on one line → inline flag set → stays inline
        let input = "<div><p>hello</p></div>";
        let output = format_ree(input, 120, 0);
        assert_eq!(output, "<div><p>hello</p></div>\n");
    }

    #[test]
    fn simple_element_block() {
        // Source has tags on separate lines → block formatting
        let input = "<div>\n<p>hello</p>\n</div>";
        let output = format_ree(input, 120, 0);
        assert_eq!(output, "<div>\n\t<p>hello</p>\n</div>\n");
    }

    #[test]
    fn ree_block() {
        let input = "{#if show}<div>yes</div>{/if}";
        let output = format_ree(input, 120, 0);        assert!(output.contains("{#if show}"));
        assert!(output.contains("<div>yes</div>"));
        assert!(output.contains("{/if}"));
    }

    #[test]
    fn ree_expression() {
        let input = "<span>{= title }</span>";
        let output = format_ree(input, 120, 0);
        assert_eq!(output, "<span>{= title}</span>\n");
    }

    #[test]
    fn void_element_self_closing() {
        let input = "<input type=\"text\" />";
        let output = format_ree(input, 120, 0);
        assert!(output.contains("<input type=\"text\" />"), "void elements should use /> syntax, got: {:?}", output);
    }

    #[test]
    fn comment_preserved() {
        let input = "<!-- hello -->";
        let output = format_ree(input, 120, 0);
        assert_eq!(output, "<!-- hello -->\n");
    }

    #[test]
    fn ree_block_inside_html_attr_preserved() {
        // Regression: {#if} blocks inside HTML tag attributes (like <option {#if cond}selected{/if}>)
        // were being parsed as separate broken attributes ({#if, name===, value, }...}selected...{/if}).
        let input = "{#each items as item }\n\t<option value=\"{= item.code }\" {#if item.selected}selected{/if}>{= item.label }</option>\n{/each}\n";
        let output = format_ree(input, 120, 0);
        assert!(            output.contains("{#if item.selected}"),
            "{{#if}} block should be preserved in output, got: {:?}",
            output
        );
        assert!(
            output.contains("{/if}"),
            "{{/if}} close should be preserved in output"
        );
        assert!(
            output.contains("selected"),
            "selected attribute inside {{#if}} block should be preserved"
        );
        // Ensure {#if} is NOT split across lines (the old bug: {#if would become
        // a separate attribute name, put on its own line)
        assert!(
            !output.contains("{#if\n"),
            "{{#if}} should NOT be split at attribute name boundary"
        );
        // Verify idempotency
        let pass2 = format_ree(&output, 120, 0);
        assert_eq!(output, pass2, "format_ree should be idempotent with Ree blocks inside HTML attrs");
    }

    #[test]
    fn doctype_preserved_and_html_root() {
        let input = "<!DOCTYPE html>\n\n<html lang=\"en\">\n\t<head>\n\t\t<meta charset=\"UTF-8\" />\n\t</head>\n</html>\n";
        let output = format_ree(input, 120, 0);
        assert!(output.starts_with("<!DOCTYPE html>"), "DOCTYPE should be first, got: {:?}", &output[..20]);
        assert!(!output.starts_with("\t<!DOCTYPE"), "DOCTYPE should not be indented");
        assert!(output.contains("\n<html"), "html should be at depth 0 after blank line");
        assert!(!output.contains("\n\t<html"), "html should NOT be indented");
    }

    #[test]
    fn inline_element_preserves_spacing_around_ree_expr() {
        // Regression: text nodes between Ree expressions were trimmed, removing
        // semantically significant spaces like "{= name } © Year {= year }" →
        // "{= name }© Year{= year }" (missing spaces around ©).
        let input = "<p>{= props.site_name } © Reepolee {= props.year }</p>";
        let output = format_ree(input, 120, 0);
        assert_eq!(
            output,
            "<p>{= props.site_name} © Reepolee {= props.year}</p>\n",
            "Spaces around copyright symbol and Ree expressions should be preserved"
        );
    }

    #[test]
    fn inline_element_spacing_idempotent() {
        // The output of the previous test must be idempotent — formatting twice
        // should produce the same result.
        let input = "<p>{= props.site_name } © Reepolee {= props.year }</p>";
        let pass1 = format_ree(input, 120, 0);
        let pass2 = format_ree(&pass1, 120, 0);
        assert_eq!(pass1, pass2, "Inline element with Ree expressions should be idempotent");
    }

    #[test]
    fn inline_element_ree_expr_with_text_before() {
        // Regression: text before a Ree expression should not lose its trailing space.
        // E.g. "<span>text {= expr }</span>" should stay as-is.
        let input = "<span>hello {= name }</span>";
        let output = format_ree(input, 120, 0);
        assert_eq!(
            output,
            "<span>hello {= name}</span>\n",
            "Space before Ree expression should be preserved"
        );
    }

    #[test]
    fn inline_element_ree_expr_with_text_after() {
        // Regression: text after a Ree expression should not lose its leading space.
        let input = "<span>{= name } world</span>";
        let output = format_ree(input, 120, 0);
        assert_eq!(
            output,
            "<span>{= name} world</span>\n",
            "Space after Ree expression should be preserved"
        );
    }

    #[test]
    fn inline_element_ree_expr_text_ree_expr() {
        // Multiple Ree expressions with text between — all spacing should be preserved.
        let input = "<span>{= a } between {= b }</span>";
        let output = format_ree(input, 120, 0);
        assert_eq!(
            output,
            "<span>{= a} between {= b}</span>\n",
            "Spacing between multiple Ree expressions should be preserved"
        );
    }

    #[test]
    fn ree_block_close_trailing_space_if() {
        // Regression: {/if } (with trailing space before }) should be consumed entirely.
        let input = "{#if show}yes{/if }";
        let output = format_ree(input, 120, 0);
        assert_eq!(output, "{#if show}\n\tyes\n{/if}\n");
    }

    #[test]
    fn ree_block_close_trailing_space_each() {
        // Same for {/each } with trailing space before }.
        let input = "{#each items as item}{= item }{/each }";
        let output = format_ree(input, 120, 0);
        assert_eq!(output, "{#each items as item}\n\t{= item}\n{/each}\n");
    }

    #[test]
    fn ree_block_close_spaced_and_trailing_space() {
        // {/ if } (space after /) with trailing space before }.
        let input = "{#if show}yes{/ if }";
        let output = format_ree(input, 120, 0);
        assert_eq!(output, "{#if show}\n\tyes\n{/if}\n");
    }

    #[test]
    fn ree_block_close_trailing_space_idempotent() {
        let input = "{#if show}yes{/if }";
        let pass1 = format_ree(input, 120, 0);
        let pass2 = format_ree(&pass1, 120, 0);
        assert_eq!(pass1, pass2, "close tag with trailing space should be idempotent");
    }

    #[test]
    fn ree_block_inline() {
        // Source has {#if} and {/if} on the same line → inline flag → stays on one line
        let input = "{#if option===\"all\"}{= selectors.all}{:else}{= option} {= selectors.per_page}{/if}";
        let output = format_ree(input, 120, 0);
        assert_eq!(
            output,
            "{#if option===\"all\"}{= selectors.all}{:else}{= option} {= selectors.per_page}{/if}\n",
            "ReeBlock with opening and closing on same line should stay inline"
        );
        let pass2 = format_ree(&output, 120, 0);
        assert_eq!(output, pass2, "inline ReeBlock should be idempotent");
    }

    #[test]
    fn ree_block_multiline() {
        // Source has {#if} and {/if} on separate lines → block formatting
        let input = "{#if option===\"all\"}\n{= selectors.all}\n{:else}\n{= option} {= selectors.per_page}\n{/if}";
        let output = format_ree(input, 120, 0);
        assert_eq!(
            output,
            "{#if option===\"all\"}\n\t{= selectors.all}\n{:else}\n\t{= option} {= selectors.per_page}\n{/if}\n",
            "Multi-line ReeBlock should be formatted with indented children"
        );
        let pass2 = format_ree(&output, 120, 0);
        assert_eq!(output, pass2, "multi-line ReeBlock should be idempotent");
    }

    #[test]
    fn inline_element_ree_call_text() {
        // Ree calls ({~ expr}) should also preserve spacing.
        let input = "<span>{~ props.greeting } user</span>";
        let output = format_ree(input, 120, 0);
        assert_eq!(
            output,
            "<span>{~ props.greeting} user</span>\n",
            "Spacing around Ree calls should be preserved"
        );
    }

    // ─── --oneline tests ──────────────────────────────────────────

    #[test]
    fn oneline_collapses_multiline_leaf_element() {
        // Multi-line element with only text content (a leaf) collapses with oneline.
        let input = "<p>\nhello\n</p>";
        let output = format_ree(input, 120, 120);
        assert_eq!(output, "<p>hello</p>\n");
    }

    #[test]
    fn oneline_does_not_collapse_parent_with_child_elements() {
        // div contains a p → not a leaf → div stays block; p is already inline so unchanged.
        let input = "<div>\n<p>hello</p>\n</div>";
        let output = format_ree(input, 120, 120);
        assert_eq!(output, "<div>\n\t<p>hello</p>\n</div>\n");
    }

    #[test]
    fn oneline_collapses_inner_leaf_but_not_outer() {
        // li elements are leaves (text only) and collapse; ul has child elements so stays block.
        let input = "<ul>\n<li>\nfoo\n</li>\n<li>\nbar\n</li>\n</ul>";
        let output = format_ree(input, 120, 120);
        assert_eq!(output, "<ul>\n\t<li>foo</li>\n\t<li>bar</li>\n</ul>\n");
    }

    #[test]
    fn oneline_keeps_block_when_too_wide() {
        // Leaf element that doesn't fit within oneline width stays multi-line.
        let input = "<p>\nthis content is quite long and exceeds the narrow wrap width\n</p>";
        let output = format_ree(input, 120, 20);
        assert!(output.contains('\n'), "Should stay multi-line when collapsed line exceeds oneline width");
        assert!(output.contains("<p>"), "Opening tag present");
        assert!(output.contains("</p>"), "Closing tag present");
    }

    #[test]
    fn oneline_with_ree_expr_collapses() {
        // Leaf element with a Ree expression collapses correctly.
        let input = "<span>\n{= title}\n</span>";
        let output = format_ree(input, 120, 120);
        assert_eq!(output, "<span>{= title}</span>\n");
    }

    #[test]
    fn oneline_zero_preserves_block_form() {
        // oneline: 0 disables collapsing — multi-line leaf elements stay multi-line.
        let input = "<p>\nhello\n</p>";
        let output = format_ree(input, 120, 0);
        assert_eq!(output, "<p>\n\thello\n</p>\n");
    }

    #[test]
    fn oneline_already_inline_stays_inline() {
        // Elements already authored on one line stay on one line regardless of oneline.
        let input = "<div><p>hello</p></div>";
        let output = format_ree(input, 120, 120);
        assert_eq!(output, "<div><p>hello</p></div>\n");
    }

    #[test]
    fn boolean_attr_with_ree_expr_stays_intact() {
        // `data-foo{~ expr }` must be kept as a single attribute token, not split
        // into separate tokens at the space inside the brace expression.
        let input = "<nav data-site-nav{~ x ? ' data-light-hero' : '' }>";
        let output = format_ree(input, 120, 0);
        assert_eq!(output, "<nav data-site-nav{~ x ? ' data-light-hero' : '' }>\n");
    }

    #[test]
    fn ree_expr_directly_after_tag_name_parses_correctly() {
        // `<details{~ expr }` — brace expression directly after tag name with no
        // space. Tag name must be `details`, not `details{~`, and closing tag
        // must render as `</details>` not `</details{~>`.
        let input = "<details{~ open ? ' open' : '' }>\nhello\n</details>";
        let output = format_ree(input, 120, 0);
        assert!(output.starts_with("<details{~ open ? ' open' : '' }>"), "opening tag preserved");
        assert!(output.contains("</details>"), "closing tag must not include the brace expression");
    }
}
