use std::fs;
use std::path::Path;
use std::collections::HashMap;
use similar::{ChangeTag, DiffOp};

use crate::full::ree_format::flatten_concat;

/// Operating mode: write files, check-only (list files), or diff (show changes).
#[derive(Clone, Copy, PartialEq)]
pub(crate) enum Mode { Write, Check, Diff }

/// Per-category collapsing limits. Controls how many members each kind of
/// syntactic structure may have before it is forced to stay multi-line.
#[derive(Clone, Copy)]
pub(crate) struct CollapseConfig {
    pub enabled: bool,
    pub max_object_members: usize,
    pub max_array_elements: usize,
    pub max_function_params: usize,
    pub max_call_args: usize,
    pub max_imports: usize,
    pub max_type_members: usize,
    /// "Soft" wrap width. A structure (call args, array/object/type members,
    /// imports) whose inline form fits within this width collapses onto one
    /// line regardless of its member count, overriding the per-category caps
    /// above. Above this width the count caps apply as usual, and `wrap_width`
    /// remains the hard ceiling. Set to 0 to disable (count caps always apply).
    pub soft_wrap_width: usize,
    /// Display width of a tab character, used when measuring line widths for
    /// wrap/collapse decisions. Because the formatter indents with hard tabs, a
    /// deeply nested line occupies more screen columns than its raw character
    /// count suggests; measuring with this value makes width decisions reflect
    /// where the last character actually lands on screen.
    pub tab_width: usize,
    /// Maximum number of `key: value` ("named") properties an object literal may
    /// have and still collapse onto one line. Shorthand (`{ a, b }`) and spread
    /// (`{ ...x }`) properties don't count. With the default `1`, a single
    /// `{ x: 1 }` stays inline but `{ x: 1, y: 2 }` always expands — inline
    /// lists of named assignments are hard to scan. Set high to disable.
    pub max_keyvalue_props: usize,
    /// When non-zero, overrides `wrap_width` as the hard ceiling for collapse
    /// decisions. A structure only collapses if its inline form fits within this
    /// many columns. Driven by the `oneline` config field.
    pub collapse_width: usize,
    /// When true, a call with a single object or array literal argument that
    /// doesn't fit inline is printed with the opening `({` on the same line as
    /// the callee and the closing `})` on its own line, instead of expanding
    /// the argument onto a separate indented line.
    pub hug_call_args: bool,
}

impl CollapseConfig {
    #[cfg(test)]
    pub(crate) fn uniform(enabled: bool, max: usize) -> Self {
        Self {
            enabled,
            max_object_members: max,
            max_array_elements: max,
            max_function_params: max,
            max_call_args: max,
            max_imports: max,
            max_type_members: max,
            // 0 isolates the count-cap path: tests that want to exercise the
            // soft-width override set it explicitly.
            soft_wrap_width: 0,
            tab_width: 4,
            max_keyvalue_props: 1,
            collapse_width: 0,
            hug_call_args: false,
        }
    }
}

/// Number of "named" (`key: value`) properties in an object literal's property
/// list. Shorthand (`{ a }`) and spread (`{ ...x }`) members are not counted —
/// they read like a plain list and stay collapsible. Used to keep objects full
/// of inline assignments (`{ a: 1, b: 2, ... }`) from collapsing onto one line.
pub(crate) fn keyvalue_prop_count(props: &[swc_core::ecma::ast::PropOrSpread]) -> usize {
    use swc_core::ecma::ast::{PropOrSpread, Prop};
    props.iter().filter(|p| match p {
        PropOrSpread::Prop(prop) => !matches!(**prop, Prop::Shorthand(_)),
        PropOrSpread::Spread(_) => false,
    }).count()
}

/// Visual column width of a single line: tabs advance to the next multiple of
/// `tab_width`; every other character counts as one column. Used for every
/// wrap/collapse width decision so that tab-indented lines are measured by the
/// on-screen position of their last character rather than their raw length.
pub(crate) fn display_width(line: &str, tab_width: usize) -> usize {
    let tab = tab_width.max(1);
    let mut col = 0;
    for ch in line.chars() {
        if ch == '\t' {
            col += tab - (col % tab);
        } else {
            col += 1;
        }
    }
    col
}

/// A placeholder entry for content extracted before SWC formatting
/// and restored afterward.
pub(crate) struct Placeholder {
    pub(crate) tag: String,
    pub(crate) original: String,
    /// Leading whitespace on the line where `/*` appears (tabs/spaces before `/*`).
    /// Used by `reindent_block_comment` to strip the original structural indentation.
    pub(crate) original_indent: String,
}

/// Pre-process source code before SWC formatting to preserve content that SWC
/// would otherwise reformat in undesirable ways.
///
/// Extracts two types of content:
/// - **Block comments** (`/* ... */` and `/** ... */`) that appear on their own line
///   (only whitespace before the `/*` and after the `*/`). These get `*/` merged
///   with the next line by SWC's codegen.
/// - **Blank lines** (empty or whitespace-only lines). These are removed by SWC's
///   codegen since it doesn't preserve blank lines between statements.
///
/// Each extracted piece is replaced with a `// __REEFMT_{type}_{id}__` placeholder
/// comment that SWC preserves. After SWC formatting, the placeholders are restored
/// to their original text.
pub(crate) fn preprocess_for_swc(code: &str) -> (String, Vec<Placeholder>) {
    let mut placeholders = Vec::new();
    let mut id_counter = 0usize;

    // ---- Pass 1: Extract block comments via character scan ----
    let pass1 = extract_block_comments(code, &mut placeholders, &mut id_counter);

    // ---- Pass 2: Extract blank lines from the result of pass 1 ----
    let pass2 = extract_blank_lines(&pass1, &mut placeholders, &mut id_counter);

    (pass2, placeholders)
}

/// Copy a single UTF-8 character from position `i` in `code` to `out`.
/// Advances `i` past the consumed bytes.
fn copy_utf8_char(code: &str, bytes: &[u8], out: &mut String, i: &mut usize) {
    if *i < bytes.len() && bytes[*i] & 0x80 == 0 {
        out.push(bytes[*i] as char);
        *i += 1;
    } else {
        let ch = code[*i..].chars().next().unwrap();
        out.push(ch);
        *i += ch.len_utf8();
    }
}

/// Returns true if the content emitted so far (the `out` buffer) ends in a context where
/// `/` starts a regex literal rather than a division operator.
/// Heuristic: after `)` or `]` it's division; after an identifier it's division unless
/// the identifier is a keyword that takes an expression (return, typeof, …); otherwise regex.
fn could_be_regex_start(out: &str) -> bool {
    let trimmed = out.trim_end();
    if trimmed.is_empty() {
        return true;
    }
    let last = trimmed.chars().last().unwrap();
    if last == ')' || last == ']' {
        return false;
    }
    if last.is_alphanumeric() || last == '_' || last == '$' {
        let last_word_start = trimmed
            .rfind(|c: char| !c.is_alphanumeric() && c != '_' && c != '$')
            .map(|p| p + 1)
            .unwrap_or(0);
        let last_word = &trimmed[last_word_start..];
        const REGEX_KEYWORDS: &[&str] = &[
            "return", "typeof", "void", "delete", "throw", "new", "in",
            "instanceof", "case", "yield", "await",
        ];
        return REGEX_KEYWORDS.contains(&last_word);
    }
    true
}

/// Scan character-by-character to find block comments on their own line
/// and replace them with `// __REEFMT_BLOCK_N__` placeholders.
/// Properly skips string literals, template literals, regex literals, and single-line comments.
fn extract_block_comments(code: &str, placeholders: &mut Vec<Placeholder>, id: &mut usize) -> String {
    let mut out = String::with_capacity(code.len());
    let bytes = code.as_bytes();
    let len = bytes.len();
    let mut i = 0;

    while i < len {

        // Skip double-quoted strings
        if bytes[i] == b'"' {
            out.push('"');
            i += 1;
            while i < len {
                let b = bytes[i];
                if b == b'\\' && i + 1 < len {
                    out.push('\\');
                    i += 1;
                    copy_utf8_char(code, bytes, &mut out, &mut i);
                } else if b == b'"' {
                    out.push('"');
                    i += 1;
                    break;
                } else {
                    copy_utf8_char(code, bytes, &mut out, &mut i);
                }
            }
            continue;
        }

        // Skip single-quoted strings
        if bytes[i] == b'\'' {
            out.push('\'');
            i += 1;
            while i < len {
                let b = bytes[i];
                if b == b'\\' && i + 1 < len {
                    out.push('\\');
                    i += 1;
                    copy_utf8_char(code, bytes, &mut out, &mut i);
                } else if b == b'\'' {
                    out.push('\'');
                    i += 1;
                    break;
                } else {
                    copy_utf8_char(code, bytes, &mut out, &mut i);
                }
            }
            continue;
        }

        // Skip regex literals /pattern/flags so backticks inside them don't
        // trigger spurious template-literal scanning.
        if bytes[i] == b'/'
            && i + 1 < len
            && bytes[i + 1] != b'/'
            && bytes[i + 1] != b'*'
            && could_be_regex_start(&out)
        {
            out.push('/');
            i += 1;
            while i < len && bytes[i] != b'\n' {
                let b = bytes[i];
                if b == b'\\' && i + 1 < len {
                    out.push('\\');
                    i += 1;
                    out.push(bytes[i] as char);
                    i += 1;
                } else if b == b'[' {
                    // character class — `/` is allowed inside, scan until `]`
                    out.push('[');
                    i += 1;
                    while i < len && bytes[i] != b']' {
                        if bytes[i] == b'\\' && i + 1 < len {
                            out.push('\\');
                            i += 1;
                            out.push(bytes[i] as char);
                            i += 1;
                        } else {
                            copy_utf8_char(code, bytes, &mut out, &mut i);
                        }
                    }
                    if i < len {
                        out.push(']');
                        i += 1;
                    }
                } else if b == b'/' {
                    out.push('/');
                    i += 1;
                    // consume flags (gimsuy)
                    while i < len && bytes[i].is_ascii_alphabetic() {
                        out.push(bytes[i] as char);
                        i += 1;
                    }
                    break;
                } else {
                    copy_utf8_char(code, bytes, &mut out, &mut i);
                }
            }
            continue;
        }

        // Skip template literals (handling nested `${}`)
        if bytes[i] == b'`' {
            out.push('`');
            i += 1;
            let mut depth = 0u32;
            while i < len {
                let b = bytes[i];
                if b == b'\\' && i + 1 < len {
                    out.push('\\');
                    i += 1;
                    out.push(bytes[i] as char); // escaped char is always ASCII
                    i += 1;
                } else if b == b'$' && i + 1 < len && bytes[i + 1] == b'{' {
                    out.push_str("${");
                    i += 2;
                    depth += 1;
                } else if b == b'}' && depth > 0 {
                    out.push('}');
                    i += 1;
                    depth -= 1;
                } else if b == b'`' && depth == 0 {
                    out.push('`');
                    i += 1;
                    break;
                } else {
                    copy_utf8_char(code, bytes, &mut out, &mut i);
                }
            }
            continue;
        }

        // Skip single-line `//` comments
        if bytes[i] == b'/' && i + 1 < len && bytes[i + 1] == b'/' {
            while i < len && bytes[i] != b'\n' {
                copy_utf8_char(code, bytes, &mut out, &mut i);
            }
            if i < len {
                out.push('\n');
                i += 1;
            }
            continue;
        }

        // Handle block comments `/* ... */`
        if bytes[i] == b'/' && i + 1 < len && bytes[i + 1] == b'*' {
            let start = i;
            i += 2;
            while i + 1 < len {
                if bytes[i] == b'*' && bytes[i + 1] == b'/' {
                    i += 2;
                    break;
                }
                i += 1;
            }
            let comment_text = &code[start..i];

            if is_block_comment_at_line_start(code, start) {
                // Capture the whitespace before `/*` on the opening line.
                let line_start = code[..start].rfind('\n').map(|p| p + 1).unwrap_or(0);
                let original_indent = code[line_start..start].to_string();

                let tag = format!("__REEFMT_BLOCK_{}__", *id);
                *id += 1;
                let line_count = comment_text.lines().count();
                for _ in 0..line_count {
                    out.push_str(&format!("// {}\n", tag));
                }
                placeholders.push(Placeholder {
                    tag,
                    original: comment_text.to_string(),
                    original_indent,
                });
                // Placeholder lines already end with \n, so skip
                // the trailing newline after */ to avoid double newlines.
                if i < len && bytes[i] == b'\n' {
                    i += 1;
                }
            } else {
                out.push_str(comment_text);
            }
            continue;
        }

        // Regular character (or start of multi-byte UTF-8)
        copy_utf8_char(code, bytes, &mut out, &mut i);
    }

    out
}

/// Check whether a block comment starting at byte offset `start` begins its
/// line: only whitespace precedes `/*` on that line.
///
/// Trailing code after `*/` (e.g. `*/ export function f()`) is intentionally
/// allowed — such comments must still be masked, because SWC's codegen
/// silently drops a leading block comment when a statement follows it on the
/// same line. The placeholder lines end in `\n`, so the trailing code flows
/// onto its own line for SWC to format, and the comment is restored verbatim.
fn is_block_comment_at_line_start(code: &str, start: usize) -> bool {
    let line_start = code[..start].rfind('\n').map(|p| p + 1).unwrap_or(0);
    let before = &code[line_start..start];
    before.trim().is_empty()
}

/// Extract blank lines (empty or whitespace-only lines) and replace each
/// with a `// __REEFMT_BLANK_N__` placeholder comment.
///
/// Operates on code that has already had block comments extracted, so
/// block-comment placeholder lines (`// __REEFMT_BLOCK_*`) are treated
/// as non-blank lines to avoid interfering with block comment spacing.
fn extract_blank_lines(code: &str, placeholders: &mut Vec<Placeholder>, id: &mut usize) -> String {
    let mut out = String::with_capacity(code.len());

    for line in code.lines() {
        let trimmed = line.trim();

        // A blank line is one that is empty or whitespace-only
        // AND is not a block-comment placeholder line
        if trimmed.is_empty() {
            let tag = format!("__REEFMT_BLANK_{}__", *id);
            *id += 1;
            out.push_str(&format!("// {}\n", tag));
            placeholders.push(Placeholder {
                tag,
                original: String::new(), // blank lines restore to empty
                original_indent: String::new(),
            });
        } else {
            out.push_str(line);
            out.push('\n');
        }
    }

    out
}

/// Restore original content from placeholders in the SWC-formatted output.
///
/// Works in two passes:
/// 1. First restore blank lines (they become empty lines, which won't interfere
///    with block comment pattern matching).
/// 2. Then restore block comments (multi-line replacement).
pub(crate) fn postprocess_from_swc(formatted: &str, placeholders: &[Placeholder]) -> String {
    // Build a map for fast lookup
    let mut by_tag: HashMap<&str, &Placeholder> = HashMap::new();
    for p in placeholders {
        by_tag.insert(&p.tag, p);
    }

    // ---- Pass 1: Restore blank lines ----
    // Blank lines are `// __REEFMT_BLANK_N__` placeholders → replace with empty lines.
    // SWC sometimes merges these inline with the preceding statement as a trailing comment
    // (e.g. `} // __REEFMT_BLANK_23__`); the loop below handles both the standalone-line
    // and inline cases by splitting on each occurrence of the marker.
    let mut result = String::with_capacity(formatted.len());
    const BLANK_MARKER: &str = "// __REEFMT_BLANK_";

    for line in formatted.lines() {
        if !line.contains(BLANK_MARKER) {
            result.push_str(line);
            result.push('\n');
            continue;
        }
        // Split the line on blank-line placeholders; each placeholder becomes an empty line.
        let mut remaining = line.trim_end();
        while let Some(pos) = remaining.find(BLANK_MARKER) {
            let before = remaining[..pos].trim_end();
            if !before.is_empty() {
                result.push_str(before);
                result.push('\n');
            }
            // Skip past `// __REEFMT_BLANK_N__` (find closing `__` after the digits)
            let after_prefix = &remaining[pos + BLANK_MARKER.len()..];
            let tag_end = after_prefix.find("__")
                .map(|p| pos + BLANK_MARKER.len() + p + 2)
                .unwrap_or(remaining.len());
            remaining = remaining[tag_end..].trim_start();
            result.push('\n'); // blank line in place of the placeholder
        }
        if !remaining.is_empty() {
            result.push_str(remaining);
            result.push('\n');
        }
    }

    // ---- Pass 2: Restore block comments ----
    // Block comments span multiple placeholder lines. We need to find contiguous
    // groups of `// __REEFMT_BLOCK_N__` lines and replace each group.
    let mut final_result = String::with_capacity(result.len());
    let lines: Vec<&str> = result.lines().collect();
    let mut i = 0;

    while i < lines.len() {
        let trimmed = lines[i].trim();
        if trimmed.starts_with("// __REEFMT_BLOCK_") {
            if let Some(tag) = extract_tag(trimmed) {
                if let Some(ph) = by_tag.get(tag.as_str()) {
                    let line_count = ph.original.lines().count();

                    // Verify we have enough consecutive lines with the same tag
                    let all_match = (0..line_count).all(|offset| {
                        i + offset < lines.len()
                            && lines[i + offset].trim() == format!("// {}", tag)
                    });

                    if all_match && !ph.original.is_empty() {
                        // Capture the indentation from the first placeholder line
                        let new_indent = &lines[i][..lines[i].len() - lines[i].trim().len()];

                        // Re-indent the original block comment to match
                        let reindented = reindent_block_comment(&ph.original, &ph.original_indent, new_indent);
                        final_result.push_str(&reindented);
                        final_result.push('\n');
                        i += line_count;
                        continue;
                    }
                }
            }
        }
        final_result.push_str(lines[i]);
        final_result.push('\n');
        i += 1;
    }

    // Remove trailing newline if formatted didn't have one
    if !formatted.ends_with('\n') && final_result.ends_with('\n') {
        final_result.pop();
    }

    final_result
}

/// Extract the tag (`__REEFMT_BLOCK_N__` or `__REEFMT_BLANK_N__`) from a
/// `// __REEFMT_...` comment line.
fn extract_tag(trimmed: &str) -> Option<String> {
    // trimmed looks like "// __REEFMT_BLOCK_0__" or "// __REEFMT_BLANK_0__"
    let trimmed = trimmed.trim();
    let after = trimmed.strip_prefix("// ")?;
    if after.starts_with("__REEFMT_") {
        Some(after.to_string())
    } else {
        None
    }
}

/// Re-indent a block comment to use the given indent string.
///
/// Detects the base indentation from the first line (e.g. `\t/**` prefixes
/// `\t`), strips it from every line, and prepends `new_indent`.
/// This preserves the internal structure (like ` * ` JSDoc markers) without
/// accumulating extra whitespace on repeated passes.
/// Re-indent a block comment to use the given indent string.
///
/// The extracted comment typically looks like:
/// ```
/// /**
///  * text
///  */
/// ```
/// where content lines already have some indentation (e.g. `\t\t`).
/// This function detects the common indentation of content lines,
/// strips it, and prepends `new_indent`.
/// Re-indent a block comment.
/// `original_indent` is the whitespace that preceded `/*` in the original source
/// (i.e. what sat between the start of the line and the `/` of `/*`).
/// `new_indent` is the whitespace preceding the `// __REEFMT_BLOCK__` placeholder
/// in the printer's output (the desired indentation level after formatting).
///
/// `ph.original` always starts at `/*` (no leading whitespace on line 0).
/// Content/closing lines (idx ≥ 1) start with `original_indent` in the source;
/// strip that prefix and prepend `new_indent` to each.
fn reindent_block_comment(comment: &str, original_indent: &str, new_indent: &str) -> String {
    let lines: Vec<&str> = comment.lines().collect();
    if lines.is_empty() {
        return comment.to_string();
    }

    let base = original_indent.len();

    let mut out = String::with_capacity(comment.len());
    for (idx, line) in lines.iter().enumerate() {
        if idx == 0 {
            // First line begins at `/*` — no leading whitespace in ph.original.
            out.push_str(new_indent);
            out.push_str(line);
        } else {
            // Content/closing lines carry original_indent as prefix; strip it.
            let stripped = if line.len() >= base { &line[base..] } else { line.trim_start() };
            if stripped.trim().is_empty() {
                // Whitespace-only line — emit blank (no trailing spaces)
            } else {
                out.push_str(new_indent);
                out.push_str(stripped);
            }
        }
        if idx < lines.len() - 1 {
            out.push('\n');
        }
    }

    out
}

/// Print a unified diff between original and formatted content.
pub(crate) fn print_diff(path: &Path, original: &str, formatted: &str) {
    let path_str = path.to_string_lossy();
    let diff = similar::TextDiff::from_lines(original, formatted);

    println!("--- a/{}", path_str);
    println!("+++ b/{}", path_str);

    for op in diff.ops() {
        match op {
            DiffOp::Equal { .. } => continue,
            _ => {
                let old_range = op.old_range();
                let new_range = op.new_range();
                let old_count = old_range.end - old_range.start;
                let new_count = new_range.end - new_range.start;
                println!(
                    "@@ -{},{} +{},{} @@",
                    old_range.start + 1,
                    if old_count == 0 { 1 } else { old_count },
                    new_range.start + 1,
                    if new_count == 0 { 1 } else { new_count },
                );
                for change in diff.iter_changes(op) {
                    match change.tag() {
                        ChangeTag::Delete => print!("-{}", change.value()),
                        ChangeTag::Insert => print!("+{}", change.value()),
                        ChangeTag::Equal => print!(" {}", change.value()),
                    }
                }
            }
        }
    }
}

/// Collapse expanded inline type literals back to a single line when they
/// fit within `max_width`.
///
/// SWC's codegen expands TS inline type literals like `{ a: string; b: number; }`
/// across multiple lines. This function detects the multi-line form and collapses
/// it back when the result fits within the max width.
///
/// Detection heuristic: a line ending with `{` where the body consists of lines
/// ending with `;` (TS type members), followed by a properly brace-matched `}`.
///
/// Runs iteratively until stable — collapsing one type literal may reveal another
/// on the same line (e.g. collapsing a parameter type literal exposes a return type
/// type literal in `Promise<{...}>`).
fn collapse_inline_type_literals(code: &str, max_width: usize, max_type_members: usize) -> String {
    let mut result = collapse_inline_type_literals_pass(code, max_width, max_type_members);
    // Run multiple passes until stable — collapsing one type literal may reveal another
    // (e.g. collapsing a parameter type literal reveals a return type type literal
    // on the same line).
    loop {
        let next = collapse_inline_type_literals_pass(&result, max_width, max_type_members);
        if next == result {
            return result;
        }
        result = next;
    }
}

/// Single pass of `collapse_inline_type_literals`. Returns the code with some
/// inline type literals collapsed. May need multiple passes for cascading cases.
fn collapse_inline_type_literals_pass(code: &str, max_width: usize, max_type_members: usize) -> String {
    let mut result = String::with_capacity(code.len());
    let lines: Vec<&str> = code.lines().collect();
    let mut i = 0;

    while i < lines.len() {
        let line = lines[i];
        let trimmed = line.trim();
        let is_comment = trimmed.starts_with("//") || trimmed.starts_with("/*");

        // Look for a line ending with `{` where the character directly before `{`
        // (ignoring trailing whitespace) is one that indicates a type position:
        //   `:` — property or mapped type: `key: {`
        //   `<` — generic argument: `Promise<{`
        //   `,` — next generic argument: `Map<K, {`
        //   `=` — type alias: `type Foo = {`
        //   `|` / `&` — union/intersection type member
        // This excludes function/class/control-flow bodies where a type KEYWORD
        // (identifier) sits between the colon and the opening brace —
        // e.g. `function foo(): string {` → before_brace ends with `g`, excluded.
        let before_brace = trimmed.strip_suffix('{').unwrap_or(trimmed).trim_end();
        let is_type_opener = matches!(
            before_brace.chars().next_back(),
            Some(':' | '<' | ',' | '=' | '|' | '&')
        );
        if trimmed.ends_with('{') && is_type_opener && !is_comment {
            // Track brace depth to handle nested type literals correctly
            let mut depth: u32 = 1;
            let mut closing_line = None;
            for j in i + 1..lines.len() {
                for (byte_pos, ch) in lines[j].char_indices() {
                    match ch {
                        '{' => depth += 1,
                        '}' => {
                            depth -= 1;
                            if depth == 0 {
                                // byte_pos is position of `}`, +1 for byte after it
                                closing_line = Some((j, byte_pos + 1));
                                break;
                            }
                        }
                        _ => {}
                    }
                }
                if closing_line.is_some() {
                    break;
                }
            }

            if let Some((end_idx, after_close_start)) = closing_line {
                let body_lines: Vec<&str> = (i + 1..end_idx)
                    .map(|j| lines[j].trim())
                    .collect();

                // Type literal check: all non-empty body lines end with `;`
                // and contain `:` before `;` (type member pattern like `key: type;`)
                let is_type_literal = !body_lines.is_empty()
                    && body_lines.iter().all(|l| {
                        l.is_empty() || (l.ends_with(';') && l.contains(':'))
                    });

                if is_type_literal {
                    // Build members list (strip trailing `;`)
                    let members: Vec<&str> = body_lines.iter()
                        .filter(|l| !l.is_empty())
                        .map(|l| l.trim_end_matches(';').trim())
                        .collect();

                    if members.len() > max_type_members {
                        // Too many members — keep multi-line
                        result.push_str(line);
                        result.push('\n');
                        i += 1;
                        continue;
                    }

                    // Get the prefix before `{` on the opening line
                    let line_prefix = &line[..line.len() - trimmed.len()];
                    let before_brace = trimmed.strip_suffix('{').unwrap_or(trimmed);

                    // Get everything after the matching `}` on the closing line
                    let rest_of_closing_line = &lines[end_idx][after_close_start..];

                    // Build inline form: `{ member; member; member; }`
                    let inner = if members.is_empty() {
                        String::new()
                    } else {
                        format!(" {}; ", members.join("; "))
                    };

                    let collapsed = format!(
                        "{}{}{{{}}}{}",
                        line_prefix,
                        before_brace,
                        inner,
                        rest_of_closing_line
                    );

                    // Check if collapsing is reasonable: measure the LOCAL type literal
                    // part (inner + braces), not the entire line which may already be
                    // long due to SWC collapsing function parameters onto one line.
                    // The max_members check above already prevents collapsing large
                    // type literals. Only reject if even the local { ... } part alone
                    // exceeds max_width (e.g. a single member with an absurdly long name).
                    let type_lit_local_len = 2 + inner.len(); // { + inner + }
                    if collapsed.len() <= max_width || type_lit_local_len <= max_width {
                        result.push_str(&collapsed);
                        result.push('\n');
                        i = end_idx + 1;
                        continue;
                    }
                }
            }
        }

        result.push_str(line);
        result.push('\n');
        i += 1;
    }

    // Remove trailing newline if original didn't have one
    if !code.ends_with('\n') && result.ends_with('\n') {
        result.pop();
    }

    result
}

/// Check if a trimmed line is a simple identifier (alphanumeric, _, $, .)
/// Used to validate object literal shorthand properties like `width, height`.
fn is_simple_ident(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| c.is_alphanumeric() || c == '_' || c == '$' || c == '.')
}

/// Returns the net brace depth of `s` (opens minus closes). Zero means all
/// braces are balanced — e.g. `attributes: {}` returns 0, `fields: {` returns 1.
fn brace_depth(s: &str) -> i32 {
    let mut depth = 0i32;
    for ch in s.chars() {
        match ch {
            '{' => depth += 1,
            '}' => depth -= 1,
            _ => {}
        }
    }
    depth
}

/// Returns true if `s` contains an odd number of unescaped backticks, meaning
/// it opens a template literal that continues on the next line.
fn has_unclosed_template_literal(s: &str) -> bool {
    let mut open = false;
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\\' { chars.next(); continue; }
        if c == '`' { open = !open; }
    }
    open
}

/// Returns true if `line` starts a multi-line block comment that isn't closed
/// on the same line (e.g. `/**` or `/* start`). Ignores `/*` that appears
/// after a `//` line-comment marker.
fn line_opens_block_comment(line: &str) -> bool {
    let t = line.trim_start();
    let Some(open) = t.find("/*") else { return false; };
    if let Some(lc) = t.find("//") {
        if lc < open { return false; }
    }
    !t[open + 2..].contains("*/")
}

/// Returns true if `line` ends a block comment (contains `*/`).
fn line_closes_block_comment(line: &str) -> bool {
    line.contains("*/")
}

/// Apply a line-by-line transformation to `code`, automatically skipping lines
/// that are inside multi-line template literals or multi-line block comments.
///
/// `f` returns `Some(replacement)` (without trailing newline) to replace the
/// current line, or `None` to copy it verbatim. Lines inside protected regions
/// are always copied verbatim regardless of what `f` returns.
fn apply_with_context<F>(code: &str, mut f: F) -> String
where
    F: FnMut(&str) -> Option<String>,
{
    let mut result = String::with_capacity(code.len() + 128);
    let mut in_template = false;
    let mut in_block_comment = false;

    for line in code.lines() {
        if in_block_comment {
            result.push_str(line);
            result.push('\n');
            if line_closes_block_comment(line) {
                in_block_comment = false;
            }
        } else if in_template {
            result.push_str(line);
            result.push('\n');
            if has_unclosed_template_literal(line) {
                in_template = false;
            }
        } else {
            let has_tpl = has_unclosed_template_literal(line);
            let opens_cmt = line_opens_block_comment(line);
            if has_tpl || opens_cmt {
                // Opening line of a multi-line region — copy verbatim, update state.
                result.push_str(line);
                result.push('\n');
                in_template = has_tpl;
                in_block_comment = opens_cmt;
            } else {
                match f(line) {
                    Some(replacement) => { result.push_str(&replacement); result.push('\n'); }
                    None => { result.push_str(line); result.push('\n'); }
                }
            }
        }
    }

    if !code.ends_with('\n') && result.ends_with('\n') {
        result.pop();
    }
    result
}

/// Check if a trimmed line is any block opener ending with `) {`.
/// Used as a negative guard so object literals aren't mistaken for blocks.
fn is_block_opener(trimmed: &str) -> bool {
    if !trimmed.ends_with('{') {
        return false;
    }
    let before_brace = trimmed[..trimmed.len() - 1].trim_end();
    before_brace.ends_with(')')
}

/// Check if a trimmed line is a block opener that should be collapsed when it
/// contains a single statement. `for`, `while`, `do`, `switch`, and all `else`
/// variants are excluded. Loops and switch obscure iteration/dispatch structure;
/// `else`/`else if` are excluded so all branches of an if/else chain are always
/// symmetric (all expanded or all collapsed — never mixed).
/// `} catch` and `} finally` are also excluded (they glue onto the preceding `}`).
fn is_collapsible_block_opener(trimmed: &str) -> bool {
    if trimmed.starts_with("} catch") || trimmed.starts_with("} finally") {
        return false;
    }
    let t = trimmed.trim_start_matches('}').trim_start();
    if t.starts_with("for ") || t.starts_with("for(")
        || t.starts_with("while ") || t.starts_with("while(")
        || t.starts_with("do {") || t == "do {"
        || t.starts_with("switch ") || t.starts_with("switch(")
        || t.starts_with("else")
    {
        return false;
    }
    is_block_opener(trimmed)
}

/// Character-scanner state carried across lines so the collapse pass can tell
/// which lines fall inside a multi-line template literal or block comment and
/// must be copied verbatim. Naive backtick counting mis-fires on backticks that
/// appear inside string literals or comments (e.g. `quote === "`"`), which
/// previously flipped the template-tracking parity and corrupted multi-line
/// template literals.
#[derive(Clone, Copy, Default)]
struct ScanState {
    in_template: bool,
    /// `${}` interpolation nesting depth inside a template literal.
    interp_depth: u32,
    in_block_comment: bool,
}

/// Scan a single line starting from `state`, returning the state at end of line.
/// In normal mode, string literals (`"..."`, `'...'`), line comments (`//`), and
/// block comments (`/* */`) are skipped so backticks inside them never toggle
/// template state. In template mode, only unescaped backticks at interpolation
/// depth 0 close the literal, and `${`/`}` adjust interpolation depth.
fn scan_line(line: &str, mut state: ScanState) -> ScanState {
    let bytes = line.as_bytes();
    let len = bytes.len();
    let mut i = 0;
    while i < len {
        let b = bytes[i];
        if state.in_block_comment {
            if b == b'*' && i + 1 < len && bytes[i + 1] == b'/' {
                state.in_block_comment = false;
                i += 2;
            } else {
                i += 1;
            }
            continue;
        }
        if state.in_template {
            if b == b'\\' {
                i += 2;
            } else if b == b'$' && i + 1 < len && bytes[i + 1] == b'{' {
                state.interp_depth += 1;
                i += 2;
            } else if b == b'}' && state.interp_depth > 0 {
                state.interp_depth -= 1;
                i += 1;
            } else if b == b'`' && state.interp_depth == 0 {
                state.in_template = false;
                i += 1;
            } else {
                i += 1;
            }
            continue;
        }
        // Normal code mode.
        match b {
            b'"' => {
                i += 1;
                while i < len {
                    let c = bytes[i];
                    if c == b'\\' { i += 2; } else if c == b'"' { i += 1; break; } else { i += 1; }
                }
            }
            b'\'' => {
                i += 1;
                while i < len {
                    let c = bytes[i];
                    if c == b'\\' { i += 2; } else if c == b'\'' { i += 1; break; } else { i += 1; }
                }
            }
            b'`' => {
                state.in_template = true;
                state.interp_depth = 0;
                i += 1;
            }
            b'/' if i + 1 < len && bytes[i + 1] == b'/' => break, // rest of line is a comment
            b'/' if i + 1 < len && bytes[i + 1] == b'*' => {
                state.in_block_comment = true;
                i += 2;
            }
            _ => i += 1,
        }
    }
    state
}

/// For each line, compute whether its START lies inside a multi-line template
/// literal or block comment (i.e. a region opened on an earlier line). Such
/// lines must be copied verbatim by the collapse pass.
fn compute_line_protection(lines: &[&str]) -> Vec<bool> {
    let mut protection = Vec::with_capacity(lines.len());
    let mut state = ScanState::default();
    for line in lines {
        protection.push(state.in_template || state.in_block_comment);
        state = scan_line(line, state);
    }
    protection
}

/// Run one pass of the collapse logic. Returns `true` if any changes were made
/// (meaning another pass may find more opportunities).
fn collapse_single_stmt_blocks_pass(code: &str, max_width: usize, collapse: &CollapseConfig, out: &mut String) -> bool {
    out.clear();
    let lines: Vec<&str> = code.lines().collect();
    let protection = compute_line_protection(&lines);
    let mut i = 0;
    let mut modified = false;

    while i < lines.len() {
        let line = lines[i];
        let trimmed = line.trim();

        // Inside a multi-line template literal or block comment: copy verbatim.
        // Protection is computed with a scanner that ignores backticks inside
        // strings and comments, so multi-line templates are never corrupted.
        if protection[i] {
            out.push_str(line);
            out.push('\n');
            i += 1;
            continue;
        }

        // ---- Single-statement block: `if (cond) {` stmt `}` ----
        if i + 2 < lines.len() && !protection[i + 1] && !protection[i + 2] && is_collapsible_block_opener(trimmed) {
            let stmt_trimmed = lines[i + 1].trim();
            let close_trimmed = lines[i + 2].trim();

            if !stmt_trimmed.is_empty()
                && close_trimmed == "}"
                && !stmt_trimmed.starts_with("//")
                && !stmt_trimmed.contains(" //")
                && !has_unclosed_template_literal(stmt_trimmed)
            {
                let prefix = &line[..line.len() - trimmed.len()];
                let before_brace = trimmed.trim_end();
                let collapsed = format!("{} {} }}", before_brace, stmt_trimmed);
                let full_line = format!("{}{}", prefix, collapsed);

                if full_line.len() <= max_width {
                    out.push_str(&full_line);
                    out.push('\n');
                    i += 3;
                    modified = true;
                    continue;
                }
            }
        }

        // ---- Object literal function param: `foo({` key:val, `})` ----
        if i + 2 < lines.len() && is_obj_lit_opener(trimmed) {
            let mut depth = 1u32;
            let mut closing = None;
            for j in i + 1..lines.len() {
                for (byte_pos, ch) in lines[j].char_indices() {
                    match ch {
                        '{' => depth += 1,
                        '}' => { depth -= 1; if depth == 0 { closing = Some((j, byte_pos + 1)); break; } }
                        _ => {}
                    }
                }
                if closing.is_some() { break; }
            }

            if let Some((end_idx, after_close)) = closing {
                let body: Vec<&str> = (i + 1..end_idx)
                    .map(|j| lines[j].trim())
                    .filter(|l| !l.is_empty())
                    .collect();

                // Count named (`key: value`) members — those that are neither a
                // spread nor a plain shorthand identifier — to keep objects full
                // of inline assignments from collapsing onto one line.
                let named_count = body.iter().filter(|l| {
                    let c = l.trim_end_matches(',').trim();
                    !c.starts_with("...") && !is_simple_ident(c)
                }).count();
                // Validate: body must contain no statements (no `;`), and each line
                // must be a key:value pair, shorthand identifier, or spread operator.
                let is_structurally_valid_obj_lit = !body.is_empty()
                    && body.iter().all(|l| {
                    let cleaned = l.trim_end_matches(',').trim();
                    !cleaned.contains(';')
                        && !cleaned.starts_with("//")
                        && !cleaned.contains(" //")
                        && (cleaned.contains(':') || cleaned.starts_with("...") || is_simple_ident(cleaned))
                });

                if is_structurally_valid_obj_lit && !(i + 1..=end_idx).any(|j| protection[j]) {
                    let prefix = &line[..line.len() - trimmed.len()];
                    let before_paren = trimmed.strip_suffix('{').unwrap_or(trimmed);
                    let after_close_str = &lines[end_idx][after_close..];
                    let members: Vec<&str> = body.iter()
                        .map(|l| l.trim_end_matches(',').trim()).collect();

                    let collapsed = format!(
                        "{}{}{{ {} }}{}",
                        prefix, before_paren, members.join(", "), after_close_str
                    );

                    let under_soft = collapse.soft_wrap_width > 0
                        && display_width(&collapsed, collapse.tab_width) <= collapse.soft_wrap_width;
                    let under_caps = body.len() <= collapse.max_object_members
                        && named_count <= collapse.max_keyvalue_props;
                    if (under_caps || under_soft) && collapsed.len() <= max_width {
                        out.push_str(&collapsed);
                        out.push('\n');
                        i = end_idx + 1;
                        modified = true;
                        continue;
                    }
                }
            }
        }

        // ---- Standalone object literal: `return { key: val }`, `const x = { key: val }` ----
        // Detects lines ending with `{` that are NOT block openers (if/for/while)
        // and NOT function call arguments.
        if i + 2 < lines.len() && trimmed.ends_with('{') && !is_block_opener(trimmed) && !is_obj_lit_opener(trimmed) {
            let mut depth = 1u32;
            let mut closing = None;
            for j in i + 1..lines.len() {
                for (byte_pos, ch) in lines[j].char_indices() {
                    match ch {
                        '{' => depth += 1,
                        '}' => { depth -= 1; if depth == 0 { closing = Some((j, byte_pos + 1)); break; } }
                        _ => {}
                    }
                }
                if closing.is_some() { break; }
            }

            if let Some((end_idx, after_close)) = closing {
                let body: Vec<&str> = (i + 1..end_idx)
                    .map(|j| lines[j].trim())
                    .filter(|l| !l.is_empty())
                    .collect();

                // Count named (`key: value`) members — those that are neither a
                // spread nor a plain shorthand identifier — to keep objects full
                // of inline assignments from collapsing onto one line.
                let named_count = body.iter().filter(|l| {
                    let c = l.trim_end_matches(',').trim();
                    !c.starts_with("...") && !is_simple_ident(c)
                }).count();
                // Validate: body must contain no statements (no `;`), and each line
                // must be a key:value pair, shorthand identifier, or spread operator.
                let is_structurally_valid_obj = !body.is_empty()
                    && body.iter().all(|l| {
                    let cleaned = l.trim_end_matches(',').trim();
                    // No statements (`;`), no nested un-collapsed braces (`{`, `}`),
                    // no comments (standalone `//` or trailing ` //`) — collapsing
                    // would make them comment out the rest of the inline expression.
                    // Allow balanced `{}` pairs (e.g. `attributes: {}`) but reject
                    // lines with unbalanced braces that would break nesting.
                    !cleaned.contains(';')
                        && !cleaned.starts_with("//")
                        && !cleaned.contains(" //")
                        && brace_depth(cleaned) == 0
                        && (cleaned.contains(':') || cleaned.starts_with("...") || is_simple_ident(cleaned))
                });

                if is_structurally_valid_obj && !(i + 1..=end_idx).any(|j| protection[j]) {
                    let prefix = &line[..line.len() - trimmed.len()];
                    let before_brace = trimmed.strip_suffix('{').unwrap_or(trimmed);
                    let after_close_str = &lines[end_idx][after_close..];
                    let members: Vec<&str> = body.iter()
                        .map(|l| l.trim_end_matches(',').trim()).collect();

                    let collapsed = format!(
                        "{}{}{{ {} }}{}",
                        prefix, before_brace, members.join(", "), after_close_str
                    );

                    let under_soft = collapse.soft_wrap_width > 0
                        && display_width(&collapsed, collapse.tab_width) <= collapse.soft_wrap_width;
                    let under_caps = body.len() <= collapse.max_object_members
                        && named_count <= collapse.max_keyvalue_props;
                    if (under_caps || under_soft) && collapsed.len() <= max_width {
                        out.push_str(&collapsed);
                        out.push('\n');
                        i = end_idx + 1;
                        modified = true;
                        continue;
                    }
                }
            }
        }

        // ---- Array literal: `const arr = [...vals]`, `fn([...args], ...)` ----
        // Detects lines ending with `[` that look like array literals
        // (not bracket access like `arr[`). The char before `[` must not
        // be alphanumeric, `_`, `)`, or `]` — which would indicate
        // property/index access.
        if i + 2 < lines.len() && trimmed.ends_with('[') {
            let before = trimmed[..trimmed.len() - 1].trim_end();
            let is_bracket_access = before.chars().last().is_some_and(|c| {
                c.is_alphanumeric() || c == '_' || c == ')' || c == ']'
            });
            if !is_bracket_access {
                let mut depth = 1u32;
                let mut closing = None;
                for j in i + 1..lines.len() {
                    for (byte_pos, ch) in lines[j].char_indices() {
                        match ch {
                            '[' => depth += 1,
                            ']' => { depth -= 1; if depth == 0 { closing = Some((j, byte_pos + 1)); break; } }
                            _ => {}
                        }
                    }
                    if closing.is_some() { break; }
                }

                if let Some((end_idx, after_close)) = closing {
                    let body: Vec<&str> = (i + 1..end_idx)
                        .map(|j| lines[j].trim())
                        .filter(|l| !l.is_empty())
                        .collect();

                    // Validate: body must contain no statements (no `;`), no nested arrays,
                    // no comments (standalone `//` or trailing ` //`) — collapsing
                    // would make them comment out the rest of the inline expression.
                    let is_structurally_valid_arr = !body.is_empty() && body.iter().all(|l| {
                        let cleaned = l.trim_end_matches(',').trim();
                        !cleaned.contains(';') && !cleaned.contains('[') && !cleaned.contains(']')
                            && !cleaned.starts_with("//")
                            && !cleaned.contains(" //")
                            && cleaned != "{" && cleaned != "}"
                    });

                    if is_structurally_valid_arr && !(i + 1..=end_idx).any(|j| protection[j]) {
                        let prefix = &line[..line.len() - trimmed.len()];
                        let before_bracket = trimmed.strip_suffix('[').unwrap_or(trimmed);
                        let after_close_str = &lines[end_idx][after_close..];
                        let members: Vec<&str> = body.iter()
                            .map(|l| l.trim_end_matches(',').trim()).collect();

                        let collapsed = format!(
                            "{}{}[{}]{}",
                            prefix, before_bracket, members.join(", "), after_close_str
                        );

                        let under_soft = collapse.soft_wrap_width > 0
                            && display_width(&collapsed, collapse.tab_width) <= collapse.soft_wrap_width;
                        let under_cap = body.len() <= collapse.max_array_elements;
                        if (under_cap || under_soft) && collapsed.len() <= max_width {
                            out.push_str(&collapsed);
                            out.push('\n');
                            i = end_idx + 1;
                            modified = true;
                            continue;
                        }
                    }
                }
            }
        }

        out.push_str(line);
        out.push('\n');
        i += 1;
    }

    if !code.ends_with('\n') && out.ends_with('\n') {
        out.pop();
    }
    modified
}

/// Collapse single-statement blocks and object literal function params onto
/// one line when they fit within `max_width`. Runs iteratively until no
/// further collapses are possible (handles nested structures like `if (x) {
///   fn({ key: val });
/// }` where the inner object-literal collapses first, then the if-block).
pub(crate) fn collapse_single_stmt_blocks(code: &str, max_width: usize, collapse: &CollapseConfig) -> String {
    let mut result = code.to_string();
    let mut buf = String::with_capacity(code.len());
    loop {
        if !collapse_single_stmt_blocks_pass(&result, max_width, collapse, &mut buf) {
            return result;
        }
        std::mem::swap(&mut result, &mut buf);
    }
}

/// Check if a trimmed line represents a function call with an object literal
/// as an argument, e.g. `foo({`, `foo(arg, {`, or `foo([...], {`.
/// Must end with `{` (after trimming), must not be a block opener like
/// `if (cond) {`, and must have `(`, `,`, or `]` before the `{`.
fn is_obj_lit_opener(trimmed: &str) -> bool {
    if !trimmed.ends_with('{') {
        return false;
    }
    if is_block_opener(trimmed) {
        return false;
    }
    // Check the character just before `{` (after trimming whitespace).
    // `(` means direct call `foo({`, `,` means after another arg `foo(a, {`,
    // `]` means after an array literal `foo([...], {`.
    let before_brace = trimmed[..trimmed.len() - 1].trim_end();
    before_brace.ends_with('(') || before_brace.ends_with(',') || before_brace.ends_with(']')
}

/// Fix SWC codegen spacing issues in try/catch/finally blocks.
///
/// SWC's built-in codegen (swc_ecma_codegen) produces incorrect spacing for
/// bare `catch` (no exception variable) and `finally` clauses. These are known
/// SWC codegen bugs:
/// - `} catch  {` → `} catch {`  (double space before `{` when no param)
/// - `} finally{` → `} finally {` (missing space before `{`)
///
/// The custom printer (swc_printer) handles these correctly; only the SWC
/// codegen path (used for .ree template script content) needs this fix.
pub(crate) fn fix_swc_try_catch_spacing(code: &str) -> String {
    code.replace("catch  {", "catch {")
        .replace("finally{", "finally {")
}

pub(crate) fn fix_arrow_spacing(code: &str) -> String {
    let mut out = String::with_capacity(code.len());
    let bytes = code.as_bytes();
    let len = bytes.len();
    let mut i = 0;

    while i < len {
        // Peek for `=>` — both bytes are ASCII
        if i + 1 < len && bytes[i] == b'=' && bytes[i + 1] == b'>' {
            // Skip if this is `<=` or `>=`
            if i > 0 && (bytes[i - 1] == b'<' || bytes[i - 1] == b'>') {
                out.push(bytes[i] as char);
                i += 1;
                continue;
            }

            // Space before `=>` if not already there
            if i > 0 && bytes[i - 1] != b' ' {
                out.push(' ');
            }

            out.push_str("=>");
            i += 2;

            // Space after `=>` (but not before a newline)
            if i < len && bytes[i] != b' ' && bytes[i] != b'\n' && bytes[i] != b'\r' {
                out.push(' ');
            }

            continue;
        }

        // Properly copy UTF-8 characters (ASCII fast path, multi-byte fallback)
        if bytes[i] & 0x80 == 0 {
            out.push(bytes[i] as char);
            i += 1;
        } else {
            let ch = code[i..].chars().next().unwrap();
            out.push(ch);
            i += ch.len_utf8();
        }
    }

    out
}



/// Check if a trimmed line contains the `function` keyword as a standalone word
/// (not inside an identifier like "myFunction" or "functionality").
fn contains_function_keyword(trimmed: &str) -> bool {
    trimmed == "function"
        || trimmed.starts_with("function ")
        || trimmed.starts_with("function(")
        || trimmed.starts_with("function<")
        || trimmed.contains(" function ")
        || trimmed.contains(" function(")
        || trimmed.contains(" function<")
        || trimmed.ends_with(" function")
}

/// Check if a trimmed line looks like a declaration that should have its
/// parameters split (arrow function expressions, class/interface/object
/// methods, or any pattern without the `function` keyword).
///
/// Heuristic: the line has `(typed params)` followed by `=>`, `{`, or `:`,
/// where the content before `(` is a name (not `.` which would indicate a
/// method call on an object). Arrow function assignments like
/// `const fn = (a: string, b: number) =>` are accepted.
///
/// The actual validation of type annotations happens after the param
/// list is parsed (in `try_split_function_params`), so this is just
/// a fast pre-check to avoid processing obviously wrong lines.
fn is_method_declaration_like(trimmed: &str) -> bool {
    if contains_function_keyword(trimmed) {
        return false; // handled by the other check
    }
    if !trimmed.contains('(') {
        return false;
    }
    // Must have content before `(` that looks like a name
    if let Some(paren_pos) = trimmed.find('(') {
        if paren_pos == 0 {
            return false;
        }
        let before = trimmed[..paren_pos].trim_end();
        // Reject method calls on an object (`obj.method(...)`)
        if before.ends_with('.') {
            return false;
        }
        // Must have at least one word character before `(`
        if !before.chars().any(|c| c.is_alphanumeric()) {
            return false;
        }
    }
    true
}

/// Split a parameter section (the text between `(` and `)`) into individual
/// parameters at commas that are NOT inside nested `<>`, `{}`, `[]`, `()`,
/// string literals, or template literals.
fn split_at_top_level_commas(s: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut depth_paren: i32 = 0;
    let mut depth_angle: i32 = 0;
    let mut depth_brace: i32 = 0;
    let mut depth_bracket: i32 = 0;
    let mut start = 0usize;
    let bytes = s.as_bytes();
    let len = bytes.len();
    let mut i = 0;

    while i < len {
        let b = bytes[i];

        // Skip double-quoted strings
        if b == b'"' {
            i += 1;
            while i < len {
                if bytes[i] == b'\\' && i + 1 < len {
                    i += 2;
                } else if bytes[i] == b'"' {
                    i += 1;
                    break;
                } else {
                    i += 1;
                }
            }
            continue;
        }

        // Skip single-quoted strings
        if b == b'\'' {
            i += 1;
            while i < len {
                if bytes[i] == b'\\' && i + 1 < len {
                    i += 2;
                } else if bytes[i] == b'\'' {
                    i += 1;
                    break;
                } else {
                    i += 1;
                }
            }
            continue;
        }

        // Skip template literals
        if b == b'`' {
            i += 1;
            let mut tmpl_depth = 0u32;
            while i < len {
                if bytes[i] == b'\\' && i + 1 < len {
                    i += 2;
                } else if bytes[i] == b'$' && i + 1 < len && bytes[i + 1] == b'{' {
                    i += 2;
                    tmpl_depth += 1;
                } else if bytes[i] == b'}' && tmpl_depth > 0 {
                    i += 1;
                    tmpl_depth -= 1;
                } else if bytes[i] == b'`' && tmpl_depth == 0 {
                    i += 1;
                    break;
                } else {
                    i += 1;
                }
            }
            continue;
        }

        match b {
            b'(' => depth_paren += 1,
            b')' => depth_paren -= 1,
            b'<' => depth_angle += 1,
            // Use saturating_sub to avoid false negatives from `=>` arrows
            // or `>=` operators that would otherwise decrement below 0.
            b'>' => {
                depth_angle = depth_angle.saturating_sub(1);
            }
            b'{' => depth_brace += 1,
            b'}' => depth_brace -= 1,
            b'[' => depth_bracket += 1,
            b']' => depth_bracket -= 1,
            b',' if depth_paren == 0
                && depth_angle == 0
                && depth_brace == 0
                && depth_bracket == 0 =>
            {
                parts.push(s[start..i].to_string());
                start = i + 1;
            }
            _ => {}
        }

        i += 1;
    }

    // Don't forget the trailing part after the last comma (or the whole string)
    if start <= len {
        parts.push(s[start..].to_string());
    }

    parts
}

/// Determine how to scan for the opening `(` of a parameter list.
///
/// For `function`-keyword declarations, we scan from after the keyword.
/// For method declarations, we scan from the start of the line.
/// Returns the byte offset to start scanning from, or `None` if the
/// line shouldn't be processed.
enum DeclType {
    FunctionKeyword,  // function keyword declaration
    MethodDeclaration, // class/interface method (no function keyword)
}

/// Check if a trimmed line is a declaration that should have its
/// params split. Returns the declaration type if yes, `None` if no.
fn classify_declaration(trimmed: &str) -> Option<DeclType> {
    if contains_function_keyword(trimmed) {
        return Some(DeclType::FunctionKeyword);
    }
    if is_method_declaration_like(trimmed) {
        return Some(DeclType::MethodDeclaration);
    }
    None
}

/// Try to split a function/method declaration's parameters one-per-line
/// when there are more than 3 params AND the line exceeds `max_width`.
///
/// This fixes SWC's tendency to collapse multi-param function signatures
/// onto a single line, making them unreadable.
///
/// If the conditions aren't met, returns `None` (no change).
fn try_split_function_params(line: &str, max_width: usize, min_params_to_split: usize) -> Option<String> {
    let trimmed = line.trim();

    let decl_type = classify_declaration(trimmed)?;
    let is_func_keyword = matches!(decl_type, DeclType::FunctionKeyword);

    // Find the opening `(` of the parameter list
    let bytes = line.as_bytes();
    let len = bytes.len();

    // Find the first `(` at the right position
    let start_offset = if is_func_keyword {
        // For `function` keyword, scan from after the keyword
        // to avoid matching type annotation parens like
        // `const fn: (x: number) => void = function(...)`.
        line.find("function").unwrap_or(0)
    } else {
        0
    };
    let mut i = start_offset;
    let mut open_paren = None;
    let mut depth = 0u32;

    while i < len {
        let b = bytes[i];

        // Skip strings
        if b == b'"' {
            i += 1;
            while i < len {
                if bytes[i] == b'\\' && i + 1 < len {
                    i += 2;
                } else if bytes[i] == b'"' {
                    i += 1;
                    break;
                } else {
                    i += 1;
                }
            }
            continue;
        }
        if b == b'\'' {
            i += 1;
            while i < len {
                if bytes[i] == b'\\' && i + 1 < len {
                    i += 2;
                } else if bytes[i] == b'\'' {
                    i += 1;
                    break;
                } else {
                    i += 1;
                }
            }
            continue;
        }
        if b == b'`' {
            i += 1;
            let mut tmpl_depth = 0u32;
            while i < len {
                if bytes[i] == b'\\' && i + 1 < len {
                    i += 2;
                } else if bytes[i] == b'$' && i + 1 < len && bytes[i + 1] == b'{' {
                    i += 2;
                    tmpl_depth += 1;
                } else if bytes[i] == b'}' && tmpl_depth > 0 {
                    i += 1;
                    tmpl_depth -= 1;
                } else if bytes[i] == b'`' && tmpl_depth == 0 {
                    i += 1;
                    break;
                } else {
                    i += 1;
                }
            }
            continue;
        }

        if b == b'(' && open_paren.is_none() {
            open_paren = Some(i);
            depth = 1;
            i += 1;
            continue;
        }

        if let Some(_) = open_paren {
            match b {
                b'(' => depth += 1,
                b')' => {
                    depth -= 1;
                    if depth == 0 {
                        let open = open_paren.unwrap();
                        let close = i;

                        // Extract the parameter section
                        let param_section = &line[open + 1..close];
                        let params = split_at_top_level_commas(param_section);

                        // For non-function declarations (methods), validate that
                        // params have type annotations to avoid matching function
                        // calls like `result = someFunc(a, b, c, d, e, f);`
                        if !is_func_keyword {
                            let has_type_annotations = params.iter().any(|p| p.trim().contains(':'));
                            if !has_type_annotations {
                                return None;
                            }
                        }

                        // Only split when params exceed the configured threshold AND line is too long
                        if params.len() <= min_params_to_split || line.len() <= max_width {
                            return None;
                        }

                        let indent = &line[..line.len() - trimmed.len()];
                        let before_paren = &line[..open + 1]; // includes the '('
                        let after_paren = &line[close..]; // from the ')' onwards

                        let mut out = String::with_capacity(
                            line.len() + params.len() * 8,
                        );
                        out.push_str(before_paren);
                        out.push('\n');
                        for (idx, param) in params.iter().enumerate() {
                            out.push_str(indent);
                            out.push('\t');
                            out.push_str(param.trim());
                            if idx < params.len() - 1 {
                                out.push_str(",\n");
                            } else {
                                // Trailing comma on last param
                                out.push_str(",\n");
                            }
                        }
                        out.push_str(indent);
                        out.push_str(after_paren);

                        return Some(out);
                    }
                }
                _ => {}
            }
        }

        i += 1;
    }

    None
}

/// Split function parameters one-per-line when a function declaration has
/// more than 3 parameters AND the line exceeds `max_width`.
///
/// Operates on each line independently — lines that don't match a function
/// signature or that already have their params split are left untouched.
fn wrap_long_function_params(code: &str, max_width: usize, min_params_to_split: usize) -> String {
    apply_with_context(code, |line| try_split_function_params(line, max_width, min_params_to_split))
}

/// Try to split a method chain that exceeds `max_width` across multiple lines.
///
/// Detects `.method(...)` calls at depth 0 (not inside nested parens, brackets,
/// angle brackets, or strings). Requires at least 2 consecutive method calls to
/// split. Each `.method(...)` goes on its own line with one additional level of
/// indentation.
///
/// For example:
/// ```
/// const grid_cols = `${Object.entries(columns).filter(...).map(...).join(" ")} auto`;
/// ```
/// becomes:
/// ```
/// const grid_cols = `${Object.entries(columns)
/// 	.filter(...)
/// 	.map(...)
/// 	.join(" ")} auto`;
/// ```
fn try_split_method_chain(line: &str, max_width: usize) -> Option<String> {
    if line.len() <= max_width {
        return None;
    }

    let trimmed = line.trim();
    let indent = &line[..line.len() - trimmed.len()];
    let bytes = line.as_bytes();
    let len = bytes.len();

    // Collect all `.method(` calls at depth 0
    // where `start` is the position of `.` and `end` is the position after matching `)`
    struct ChainCall {
        dot_pos: usize,
        end_pos: usize,
    }
    let mut calls: Vec<ChainCall> = Vec::new();
    let mut i = 0;
    let mut depth_paren: i32 = 0;
    let mut depth_angle: i32 = 0;
    let mut depth_bracket: i32 = 0;

    // Track template literal depth for proper scanning
    // 0 = not in template; 1 = in template TEXT; 2+ = in template EXPRESSION
    let mut template_depth: u32 = 0;

    while i < len {
        let b = bytes[i];

        // Skip double-quoted strings
        if b == b'"' {
            i += 1;
            while i < len {
                if bytes[i] == b'\\' && i + 1 < len {
                    i += 2;
                } else if bytes[i] == b'"' {
                    i += 1;
                    break;
                } else {
                    i += 1;
                }
            }
            continue;
        }

        // Skip single-quoted strings
        if b == b'\'' {
            i += 1;
            while i < len {
                if bytes[i] == b'\\' && i + 1 < len {
                    i += 2;
                } else if bytes[i] == b'\'' {
                    i += 1;
                    break;
                } else {
                    i += 1;
                }
            }
            continue;
        }

        // Handle template literals — track depth so we process the
        // expression part `${...}` normally but skip the text part.
        if b == b'`' {
            if template_depth == 0 {
                template_depth = 1; // entering template TEXT
                i += 1;
                // Skip template text until `${` or closing `` ` ``
                while i < len {
                    if bytes[i] == b'\\' && i + 1 < len {
                        i += 2;
                    } else if bytes[i] == b'$' && i + 1 < len && bytes[i + 1] == b'{' {
                        // Entering template EXPRESSION — break back to main loop
                        template_depth = 2; // in expression (1 for text + 1 for ${})
                        i += 2; // skip ${
                        // We DO NOT increment depth_brace here because we're
                        // not inside a regular brace — we're in a template.
                        // The expression's `{` is already consumed.
                        break;
                    } else if bytes[i] == b'`' {
                        template_depth = 0;
                        i += 1;
                        break;
                    } else {
                        i += 1;
                    }
                }
                continue;
            } else if template_depth > 0 {
                // Template closed
                template_depth = 0;
                i += 1;
                continue;
            }
        }

        // When in template expression, track `}` to return to text
        if template_depth == 2 && b == b'}' {
            // Check if this closes the template expression
            // We need to track brace depth within the expression
            // Using depth_brace: decremented by `}`, then check
            // if we've closed all braces opened in the expression
            template_depth = 1; // back to template text
            i += 1;
            // Now skip template text until ${ or `
            while i < len {
                if bytes[i] == b'\\' && i + 1 < len {
                    i += 2;
                } else if bytes[i] == b'$' && i + 1 < len && bytes[i + 1] == b'{' {
                    template_depth = 2; // back in expression
                    i += 2;
                    break;
                } else if bytes[i] == b'`' {
                    template_depth = 0;
                    i += 1;
                    break;
                } else {
                    i += 1;
                }
            }
            continue;
        }

        // Skip template expression mode's `{` for tracking
        // When in template expression and we see `{`, increment depth
        if template_depth >= 2 && b == b'{' {
            // This braces the expression depth tracking
            template_depth += 1;
            i += 1;
            continue;
        }
        if template_depth >= 2 && b == b'}' {
            // Already handled above for depth==2 case
            // For depth > 2, this is a nested brace inside the expression
            template_depth -= 1;
            i += 1;
            continue;
        }

        // Skip processing when in template TEXT (not in expression)
        if template_depth == 1 {
            i += 1;
            continue;
        }

        // Track depth for nested structures
        match b {
            b'(' => depth_paren += 1,
            b')' => depth_paren -= 1,
            b'<' => depth_angle += 1,
            b'>' => {
                depth_angle = depth_angle.saturating_sub(1);
            }
            b'[' => depth_bracket += 1,
            b']' => depth_bracket -= 1,
            _ => {}
        }

        // Look for `.` at depth 0 (not nested inside parens/brackets/angles)
        // Also skip `.` when in template text (template_depth == 1)
        if b == b'.'
            && depth_paren == 0
            && depth_angle == 0
            && depth_bracket == 0
        {
            let after = i + 1;
            if after < len && (bytes[after].is_ascii_alphabetic() || bytes[after] == b'_' || bytes[after] == b'$') {
                // Find end of method name
                let mut j = after;
                while j < len && (bytes[j].is_ascii_alphanumeric() || bytes[j] == b'_' || bytes[j] == b'$') {
                    j += 1;
                }
                // Skip whitespace before `(`
                while j < len && bytes[j] == b' ' {
                    j += 1;
                }
                if j < len && bytes[j] == b'(' {
                    // Find matching `)` — track depth of parens
                    let mut paren_depth = 1u32;
                    let mut k = j + 1;
                    while k < len && paren_depth > 0 {
                        // Skip strings inside the method call
                        if bytes[k] == b'"' {
                            k += 1;
                            while k < len {
                                if bytes[k] == b'\\' && k + 1 < len {
                                    k += 2;
                                } else if bytes[k] == b'"' {
                                    k += 1;
                                    break;
                                } else {
                                    k += 1;
                                }
                            }
                            continue;
                        }
                        if bytes[k] == b'\'' {
                            k += 1;
                            while k < len {
                                if bytes[k] == b'\\' && k + 1 < len {
                                    k += 2;
                                } else if bytes[k] == b'\'' {
                                    k += 1;
                                    break;
                                } else {
                                    k += 1;
                                }
                            }
                            continue;
                        }
                        if bytes[k] == b'(' {
                            paren_depth += 1;
                        } else if bytes[k] == b')' {
                            paren_depth -= 1;
                        }
                        k += 1;
                    }
                    if paren_depth == 0 {
                        calls.push(ChainCall {
                            dot_pos: i,
                            end_pos: k, // position after `)`
                        });
                        i = k;
                        continue;
                    }
                }
            }
        }

        i += 1;
    }

    // Group calls into contiguous chains: two consecutive calls belong to the
    // same chain only if the text between them (call[i].end_pos..call[i+1].dot_pos)
    // is whitespace-only. Binary operators like `||`, `&&`, `+`, identifiers, etc.
    // between calls mean they are on separate chains and must NOT be merged.
    // Find the first contiguous chain with ≥ 3 calls to split.
    let (chain_start, chain_end) = {
        let mut start = 0;
        let mut found = None;
        while start < calls.len() {
            let mut end = start + 1;
            while end < calls.len() {
                let gap = &line[calls[end - 1].end_pos..calls[end].dot_pos];
                if !gap.chars().all(|c| c.is_whitespace()) {
                    break;
                }
                end += 1;
            }
            if end - start >= 3 {
                found = Some((start, end));
                break;
            }
            start = end;
        }
        match found {
            Some(r) => r,
            None => return None,
        }
    };

    // Build output: first line has everything up to the SECOND `.` of the chain,
    // then `.method(args)` from the second chain call onwards each on their own
    // indented line. Everything after the last chain call stays on the last line.
    let mut out = String::with_capacity(line.len() + (chain_end - chain_start) * 8);

    // Everything before the second call of the chain stays on the first line
    out.push_str(&line[..calls[chain_start + 1].dot_pos]);
    out.push('\n');

    // Middle calls of the chain on their own indented lines
    let last_chain_idx = chain_end - 1;
    for call in calls[chain_start + 1..last_chain_idx].iter() {
        out.push_str(indent);
        out.push('\t');
        out.push_str(&line[call.dot_pos..call.end_pos]);
        out.push('\n');
    }

    // Last chain call plus everything after it (trailing operators, etc.) on one line
    out.push_str(indent);
    out.push('\t');
    out.push_str(&line[calls[last_chain_idx].dot_pos..]);

    Some(out)
}

/// Split method chains that exceed `max_width` across multiple lines.
///
/// Operates line-by-line. Lines that don't fit or don't have method chains
/// are left untouched.
fn wrap_long_method_chains(code: &str, max_width: usize) -> String {
    apply_with_context(code, |line| try_split_method_chain(line, max_width))
}

pub(crate) fn format_css_content(input: &str, wrap_width: usize) -> String {
    use malva::Syntax;
    use malva::config::{FormatOptions, LayoutOptions, LanguageOptions, LineBreak};
    let options = FormatOptions {
        layout: LayoutOptions {
            print_width: wrap_width,
            use_tabs: true,
            indent_width: 1,
            line_break: LineBreak::Lf,
        },
        language: LanguageOptions::default(),
    };
    match malva::format_text(input, Syntax::Css, &options) {
        Ok(formatted) => formatted,
        Err(_) => input.to_string(),
    }
}

/// Format standalone code content (TS/JS/CSS) using native SWC, no subprocess needed.
/// Set `REEFMT_DEBUG=1` to log each pipeline stage to stderr.
pub(crate) fn format_code_content(
    content: &str,
    ext: &str,
    wrap_width: usize,
    collapse: CollapseConfig,
    remove_unused: bool,
) -> String {
    let normalized = content.replace("\r\n", "\n");
    let debug = std::env::var("REEFMT_DEBUG").is_ok();

    macro_rules! log_stage {
        ($label:expr, $value:expr) => {
            if debug {
                eprintln!("\n=== [reefmt debug] {} ===\n{}", $label, $value);
            }
        };
    }

    let formatted = match ext {
        "ts" | "js" => {
            let flattened = flatten_concat(&normalized);
            let (preprocessed, placeholders) = preprocess_for_swc(&flattened);
            log_stage!("after preprocess_for_swc", preprocessed);
            let custom_formatted = crate::full::swc_printer::format_js_with_printer(
                &preprocessed, "\t", wrap_width, collapse, remove_unused,
            );
            log_stage!("after format_js_with_printer", custom_formatted);
            // If SWC produced no output but placeholders exist (e.g. a file that is
            // only a block comment with no code), restore from the preprocessed input
            // directly so the block comments are not silently dropped.
            let custom_formatted = if custom_formatted.trim().is_empty() && !placeholders.is_empty() {
                preprocessed.clone()
            } else {
                custom_formatted
            };
            // Always run postprocess so it can also clean up stray __REEFMT_BLANK_ tokens
            // that leaked into files from a previous buggy reefmt run.
            let restored = postprocess_from_swc(&custom_formatted, &placeholders);
            log_stage!("after postprocess_from_swc", restored);
            // Text-based post-passes: the printer emits function signatures and
            // method chains on a single line; re-apply the line-wrapping passes
            // (and type-literal / single-statement collapsing) that the old
            // codegen path used, so long lines still wrap.
            let effective_width = if collapse.collapse_width > 0 { collapse.collapse_width } else { wrap_width };
            let collapsed = collapse_inline_type_literals(&restored, effective_width, collapse.max_type_members);
            log_stage!("after collapse_inline_type_literals", collapsed);
            let params_split = wrap_long_function_params(&collapsed, effective_width, collapse.max_function_params);
            log_stage!("after wrap_long_function_params", params_split);
            let chains_split = wrap_long_method_chains(&params_split, effective_width);
            log_stage!("after wrap_long_method_chains", chains_split);
            if collapse.enabled {
                let final_result = collapse_single_stmt_blocks(&chains_split, effective_width, &collapse);
                log_stage!("after collapse_single_stmt_blocks", final_result);
                final_result
            } else {
                chains_split
            }
        }
        "css" => format_css_content(&normalized, wrap_width),
        _ => normalized.clone(),
    };

    if !formatted.is_empty() && !formatted.ends_with('\n') {
        format!("{}\n", formatted)
    } else {
        formatted
    }
}

/// Format a standalone code file (TS, JS, CSS). Returns `true` if modified.
pub(crate) fn format_code_file(path: &Path, mode: Mode, wrap_width: usize, collapse: CollapseConfig, remove_unused: bool) -> bool {
    let content = match fs::read_to_string(path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Error reading {}: {}", path.display(), e);
            return false;
        }
    };

    let normalized = content.replace("\r\n", "\n");
    let ext = path.extension().and_then(|s| s.to_str()).unwrap_or("");
    let write_content = format_code_content(&normalized, ext, wrap_width, collapse, remove_unused);

    // AST safety check: abort if semantics changed (only for TS/JS files)
    if matches!(ext, "ts" | "js") {
        if let Err(msg) = crate::full::ast_check::verify_semantics_preserved(&normalized, &write_content) {
            eprintln!(
                "\n\x1b[1;31mFATAL: reefmt would corrupt {}:\x1b[0m {}",
                path.display(),
                msg
            );
            eprintln!("File NOT written. Please report this as a bug.");
            eprintln!("  effective config: wrapWidth={} collapseMaxMembers={} collapseEnabled={} removeUnusedImports={}",
                wrap_width, collapse.max_object_members, collapse.enabled, remove_unused);
            eprintln!("\n--- diff (original → formatted) ---");
            print_diff(path, &normalized, &write_content);
            if std::env::var("REEFMT_DEBUG").is_ok() {
                eprintln!("\n=== [reefmt debug] full formatted output ===\n{}", write_content);
            }
            return false;
        }
    }

    // Comment preservation check: abort if any comment was lost (all file types)
    if let Err(msg) = crate::full::ast_check::verify_comments_preserved(&normalized, &write_content, ext) {
        eprintln!(
            "\n\x1b[1;31mFATAL: reefmt dropped a comment in {}:\x1b[0m {}",
            path.display(),
            msg
        );
        eprintln!("File NOT written. Please report this as a bug.");
        eprintln!("  effective config: wrapWidth={} collapseMaxMembers={} collapseEnabled={} removeUnusedImports={}",
            wrap_width, collapse.max_object_members, collapse.enabled, remove_unused);
        eprintln!("\n--- diff (original → formatted) ---");
        print_diff(path, &normalized, &write_content);
        if std::env::var("REEFMT_DEBUG").is_ok() {
            eprintln!("\n=== [reefmt debug] full formatted output ===\n{}", write_content);
        }
        return false;
    }

    if write_content == normalized {
        return false;
    }

    match mode {
        Mode::Write => {
            match fs::write(path, &write_content) {
                Ok(_) => eprintln!("\r\x1b[KFormatted: {}", path.display()),
                Err(e) => eprintln!("Error writing {}: {}", path.display(), e),
            }
        }
        Mode::Check => {
            eprintln!("Would format: {}", path.display());
        }
        Mode::Diff => {
            print_diff(path, &normalized, &write_content);
        }
    }

    true
}

// Note: reefmt's `format_file` (which drove per-file IO from `crate::ReeConfig`)
// is intentionally not vendored — reettier's `main.rs` owns file discovery and
// IO, and the `full` engine is reached only through the content entry points
// (`format_ree_content` / `format_code_content`).

#[cfg(test)]
mod tests {
    use std::env;
    use super::*;

    #[test]
    fn check_mode_does_not_modify_ree_file() {
        let dir = env::temp_dir().join("reefmt_test_check_mode");
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("test.ree");
        let unformatted = "{#if show}\n<div>\n{=title}\n</div>\n{/if}";
        fs::write(&path, unformatted).unwrap();

        let modified = crate::full::ree_format::format_ree_file(&path, Mode::Check, 120, 0, CollapseConfig::uniform(true, 3), false);
        assert!(modified, "Check mode should return true when file would change");
        let content_after = fs::read_to_string(&path).unwrap();
        assert_eq!(content_after, unformatted, "Check mode should not modify the file");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn check_mode_reports_no_change_for_formatted_ree_file() {
        let dir = env::temp_dir().join(format!("reefmt_test_check_fmt_{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("test.ree");
        let content = "<span>text</span>\n";
        fs::write(&path, content).unwrap();

        let modified = crate::full::ree_format::format_ree_file(&path, Mode::Check, 120, 0, CollapseConfig::uniform(true, 3), false);
        assert!(!modified, "Check mode should return false for already-formatted file (modified={})", modified);

        let content_after = fs::read_to_string(&path).unwrap();
        assert_eq!(content_after, content, "Check mode should not modify the file");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn diff_mode_does_not_modify_ree_file() {
        let dir = env::temp_dir().join("reefmt_test_diff_mode");
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("test.ree");
        let unformatted = "{#if show}\n<div>\n{=title}\n</div>\n{/if}";
        fs::write(&path, unformatted).unwrap();

        let modified = crate::full::ree_format::format_ree_file(&path, Mode::Diff, 120, 0, CollapseConfig::uniform(true, 3), false);
        assert!(modified, "Diff mode should return true when file would change");
        let content_after = fs::read_to_string(&path).unwrap();
        assert_eq!(content_after, unformatted, "Diff mode should not modify the file");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn write_mode_modifies_ree_file() {
        let dir = env::temp_dir().join("reefmt_test_write_mode");
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("test.ree");
        let unformatted = "{#if show}\n<div>\n{=title}\n</div>\n{/if}";
        fs::write(&path, unformatted).unwrap();

        let modified = crate::full::ree_format::format_ree_file(&path, Mode::Write, 120, 0, CollapseConfig::uniform(true, 3), false);
        assert!(modified, "Write mode should return true when file changes");
        let content_after = fs::read_to_string(&path).unwrap();
        assert_ne!(content_after, unformatted, "Write mode should modify the file");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn diff_mode_formatted_ree_file_returns_false() {
        let dir = env::temp_dir().join(format!("reefmt_test_diff_fmt_{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("test.ree");
        let content = "<span>text</span>\n";
        fs::write(&path, content).unwrap();

        let modified = crate::full::ree_format::format_ree_file(&path, Mode::Diff, 120, 0, CollapseConfig::uniform(true, 3), false);
        assert!(!modified, "Diff mode should return false for already-formatted file");
        let content_after = fs::read_to_string(&path).unwrap();
        assert_eq!(content_after, content, "Diff mode should not modify the file");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn check_mode_code_file_does_not_modify() {
        let dir = env::temp_dir().join("reefmt_test_code_check");
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("test.ts");
        let content = "const x = 1;\n";
        fs::write(&path, content).unwrap();

        let _modified = format_code_file(&path, Mode::Check, 180, CollapseConfig::uniform(true, 3), false);
        let content_after = fs::read_to_string(&path).unwrap();
        assert_eq!(content_after, content, "Check mode should not modify the code file");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn diff_mode_code_file_missing_returns_false() {
        let path = Path::new("/tmp/nonexistent_file_reefmt_diff_test.ts");
        let modified = format_code_file(path, Mode::Diff, 180, CollapseConfig::uniform(true, 3), false);
        assert!(!modified, "format_code_file Diff should return false for missing file");
    }

    #[test]
    fn check_mode_ree_file_missing_returns_false() {
        let path = Path::new("/tmp/nonexistent_file_reefmt_test.ree");
        let modified = crate::full::ree_format::format_ree_file(path, Mode::Check, 120, 0, CollapseConfig::uniform(true, 3), false);
        assert!(!modified, "format_ree_file should return false for missing file");
    }

    #[test]
    fn format_code_content_js_uses_swc() {
        let src = "const x=1;const y=2;";
        let result = format_code_content(src, "js", 180, CollapseConfig::uniform(true, 3), false);
        assert!(result.contains("const x = 1;"), "SWC should format JS: got {:?}", result);
    }

    #[test]
    fn idempotent_format_code_content_js() {
        let src = "const x = 1;\n";
        let pass1 = format_code_content(src, "js", 180, CollapseConfig::uniform(true, 3), false);
        let pass2 = format_code_content(&pass1, "js", 180, CollapseConfig::uniform(true, 3), false);
        assert_eq!(pass1, pass2, "format_code_content should be idempotent for JS");
    }

    #[test]
    fn idempotent_format_code_content_non_ascii_comment() {
        let src = "// Café naïve — ščüéø\nconst x = 1;\n";
        let pass1 = format_code_content(src, "js", 180, CollapseConfig::uniform(true, 3), false);
        let pass2 = format_code_content(&pass1, "js", 180, CollapseConfig::uniform(true, 3), false);
        assert_eq!(pass1, pass2,
            "format_code_content should be idempotent with non-ASCII chars");
    }

    #[test]
    fn format_code_content_css_formats() {
        let src = "body{color:red}\n";
        let result = format_code_content(src, "css", 180, CollapseConfig::uniform(true, 3), false);
        assert!(result.contains("color: red"), "CSS should be formatted: {result}");
    }

    #[test]
    fn preserves_block_comments() {
        let src = "/**\n * doc\n */\nexport const x = 1;\n";
        let result = format_code_content(src, "ts", 180, CollapseConfig::uniform(true, 3), false);
        assert!(result.contains("/**"), "block comment /** should be preserved");
        assert!(result.contains("* doc\n"), "block comment content should be preserved (space before * is normalized)");
        assert!(result.contains("*/\nexport"), "*/ should be on its own line before export");
    }

    #[test]
    fn jsdoc_comment_inside_type_preserved() {
        let src = "export type Opts = {\n\t/**\n\t * A doc comment.\n\t */\n\tcrop?: number;\n};\n";
        let result = format_code_content(src, "ts", 180, CollapseConfig::uniform(true, 3), false);
        assert!(result.contains("/**"), "JSDoc /** inside type literal should be preserved");
        assert!(result.contains("* A doc comment."), "JSDoc body should be preserved");
        assert!(result.contains("crop?"), "property after JSDoc should be preserved");
    }

    #[test]
    fn inline_comment_on_type_member_preserved() {
        let src = "export type Opts = {\n\tleft: number; // my comment\n\ttop: number;\n};\n";
        let result = format_code_content(src, "ts", 180, CollapseConfig::uniform(true, 3), false);
        assert!(result.contains("// my comment"), "inline trailing comment on type member should be preserved");
    }

    #[test]
    fn preserves_blank_lines_between_statements() {
        let src = "export interface A {\n\tx: number;\n}\n\nexport interface B {\n\ty: number;\n}\n";
        let result = format_code_content(src, "ts", 180, CollapseConfig::uniform(true, 3), false);
        assert!(result.contains("}\n\nexport"), "blank line between interfaces should be preserved");
    }

    #[test]
    fn inline_blank_placeholder_restored() {
        // Simulate the case where SWC merges a blank-line placeholder inline as a
        // trailing comment: `} // __REEFMT_BLANK_23__`
        // postprocess_from_swc must split this into the code line + a blank line.
        use super::postprocess_from_swc;
        use super::Placeholder;
        let ph = Placeholder {
            tag: "__REEFMT_BLANK_23__".to_string(),
            original: String::new(),
            original_indent: String::new(),
        };
        let formatted = "export function foo() {\n} // __REEFMT_BLANK_23__\nexport function bar() {}\n";
        let result = postprocess_from_swc(formatted, &[ph]);
        assert_eq!(
            result,
            "export function foo() {\n}\n\nexport function bar() {}\n",
            "inline blank placeholder should become a blank line after the code"
        );
    }

    #[test]
    fn inline_blank_placeholder_with_trailing_comment() {
        // Simulate: `import { db } from "$config/db"; // __REEFMT_BLANK_0__ // Add custom queries here.`
        use super::postprocess_from_swc;
        use super::Placeholder;
        let ph = Placeholder {
            tag: "__REEFMT_BLANK_0__".to_string(),
            original: String::new(),
            original_indent: String::new(),
        };
        let formatted =
            "import { db } from \"$config/db\"; // __REEFMT_BLANK_0__ // Add custom queries here.\n";
        let result = postprocess_from_swc(formatted, &[ph]);
        assert_eq!(
            result,
            "import { db } from \"$config/db\";\n\n// Add custom queries here.\n",
            "inline blank placeholder should split the line and restore the trailing comment"
        );
    }

    #[test]
    fn inline_block_comment_not_extracted() {
        let src = "const x = 1; /* inline */\n";
        let result = format_code_content(src, "ts", 180, CollapseConfig::uniform(true, 3), false);
        assert!(result.contains("/* inline */"), "inline block comments should stay inline");
    }

    #[test]
    fn block_comment_in_string_not_extracted() {
        let src = "const s = \"/* not a comment */\";\n";
        let result = format_code_content(src, "ts", 180, CollapseConfig::uniform(true, 3), false);
        assert!(result.contains("/* not a comment */"), "comments inside strings should be preserved");
    }

    #[test]
    fn single_line_block_comment_own_line() {
        let src = "/* standalone */\nexport const x = 1;\n";
        let result = format_code_content(src, "ts", 180, CollapseConfig::uniform(true, 3), false);
        assert!(result.contains("/* standalone */\nexport"), "standalone block comment should be on its own line");
    }

    #[test]
    fn idempotent_with_block_comments_and_blank_lines() {
        let src = "/**\n * Translation helpers\n */\n\n// ─── Types ─────────────────────────────────────────────────────\n\nexport interface TranslationRow {\n\tid: number;\n\tlang: string;\n}\n\nexport interface GroupInfo {\n\tnamespace: string;\n\tchild_keys: string[];\n}\n";
        let pass1 = format_code_content(src, "ts", 180, CollapseConfig::uniform(true, 3), false);
        let pass2 = format_code_content(&pass1, "ts", 180, CollapseConfig::uniform(true, 3), false);
        assert_eq!(pass1, pass2, "output should be idempotent with block comments and blank lines");
    }

    #[test]
    fn idempotent_block_comment_inside_object_literal() {
        // Regression: block comments nested inside an object literal (e.g.
        // inside `return { /** ... */ method() { } }`) kept gaining indentation
        // on each pass because SWC re-indented placeholder lines and the
        // content lines of the comment had extra spacing that accumulated.
        let src = "export function create_cache(redis_client: typeof default_redis) {\n\treturn {\n\t\t/**\n\t\t * Wraps a search query with Redis caching and dependency tracking.\n\t\t * On cache miss, stores the result and registers it in dependency sets\n\t\t * for each table in `view_deps`.\n\t\t *\n\t\t * On Redis error, falls back to `query_fn()` directly with a warning.\n\t\t */\n\t\tasync search<T>(\n\t\t\troute: string,\n\t\t\tparams: Record<string, unknown>,\n\t\t\tview_deps: string[],\n\t\t\tquery_fn: () => Promise<T>,\n\t\t): Promise<T> {\n\t\t\treturn cached_query(route, params, view_deps, query_fn);\n\t\t}\n\t};\n}\n";
        let pass1 = format_code_content(src, "ts", 180, CollapseConfig::uniform(true, 3), false);
        let pass2 = format_code_content(&pass1, "ts", 180, CollapseConfig::uniform(true, 3), false);
        assert_eq!(pass1, pass2, "block comment inside object literal should be idempotent");
    }

    #[test]
    fn line_comment_before_array_element_preserved() {
        let src = "const x = [\n\t// first\n\t1,\n\t// second\n\t2,\n];\n";
        let result = format_code_content(src, "ts", 180, CollapseConfig::uniform(true, 3), false);
        assert!(result.contains("// first"), "// comment before array element must be preserved");
        assert!(result.contains("// second"), "// comment before array element must be preserved");
    }

    #[test]
    fn line_comment_before_array_element_idempotent() {
        let src = "const x = [\n\t// Pages\n\t{ url: \"/\" },\n\t// System\n\t{ url: \"/system\" },\n];\n";
        let pass1 = format_code_content(src, "ts", 180, CollapseConfig::uniform(true, 3), false);
        let pass2 = format_code_content(&pass1, "ts", 180, CollapseConfig::uniform(true, 3), false);
        assert!(pass1.contains("// Pages"), "// Pages comment must be in output");
        assert!(pass1.contains("// System"), "// System comment must be in output");
        assert_eq!(pass1, pass2, "array with line comments should format idempotently");
    }

    #[test]
    fn line_comment_at_end_of_object_forces_expansion() {
        // Comments after the last prop (before closing }) must be preserved
        // and must force the object to expand rather than collapse inline.
        let src = "export const routes = {\n\t...a,\n\t...b,\n\t// GENERATED:start\n\t// GENERATED:end\n};\n";
        let result = format_code_content(src, "ts", 180, CollapseConfig::uniform(true, 3), false);
        assert!(result.contains("// GENERATED:start"), "trailing comment in object must be preserved");
        assert!(result.contains("// GENERATED:end"), "trailing comment in object must be preserved");
        assert!(!result.contains("...a, ...b"), "object with trailing comments must not be collapsed inline");
    }

    #[test]
    fn line_comment_at_end_of_object_idempotent() {
        let src = "export const routes = {\n\t...a,\n\t...b,\n\t// GENERATED:start\n\t// GENERATED:end\n};\n";
        let pass1 = format_code_content(src, "ts", 180, CollapseConfig::uniform(true, 3), false);
        let pass2 = format_code_content(&pass1, "ts", 180, CollapseConfig::uniform(true, 3), false);
        assert_eq!(pass1, pass2, "object with trailing line comments should format idempotently");
    }

    #[test]
    fn line_comment_before_class_member_preserved() {
        let src = "class Foo {\n\t// a comment\n\tbar() {}\n}\n";
        let result = format_code_content(src, "ts", 180, CollapseConfig::uniform(true, 3), false);
        assert!(result.contains("// a comment"), "// comment before class member must be preserved");
    }

    #[test]
    fn line_comment_before_class_member_idempotent() {
        let src = "class Foo {\n\t// getter\n\tget value() { return this._v; }\n\t// setter\n\tset value(v: number) { this._v = v; }\n}\n";
        let pass1 = format_code_content(src, "ts", 180, CollapseConfig::uniform(true, 3), false);
        let pass2 = format_code_content(&pass1, "ts", 180, CollapseConfig::uniform(true, 3), false);
        assert!(pass1.contains("// getter"), "// getter comment must survive formatting");
        assert!(pass1.contains("// setter"), "// setter comment must survive formatting");
        assert_eq!(pass1, pass2, "class with line comments before members should format idempotently");
    }

    #[test]
    fn line_comment_at_end_of_block_preserved() {
        let src = "function foo() {\n\tconst x = 1;\n\t// trailing comment\n}\n";
        let result = format_code_content(src, "ts", 180, CollapseConfig::uniform(true, 3), false);
        assert!(result.contains("// trailing comment"), "// comment before closing brace must be preserved");
    }

    #[test]
    fn line_comment_at_end_of_block_idempotent() {
        let src = "function foo() {\n\tconst x = 1;\n\t// trailing comment\n}\n";
        let pass1 = format_code_content(src, "ts", 180, CollapseConfig::uniform(true, 3), false);
        let pass2 = format_code_content(&pass1, "ts", 180, CollapseConfig::uniform(true, 3), false);
        assert_eq!(pass1, pass2, "block with trailing line comment should format idempotently");
    }

    #[test]
    fn line_comment_before_interface_member_preserved() {
        let src = "interface Foo {\n\t// id field\n\tid: number;\n\t// name field\n\tname: string;\n}\n";
        let result = format_code_content(src, "ts", 180, CollapseConfig::uniform(true, 3), false);
        assert!(result.contains("// id field"), "// comment before interface member must be preserved");
        assert!(result.contains("// name field"), "// comment before interface member must be preserved");
    }

    // ─── collapse_single_stmt_blocks tests ────────────────────────

    #[test]
    fn collapse_simple_if_block() {
        let src = "\tif (cond) {\n\t\tdoSomething();\n\t}\n";
        let result = collapse_single_stmt_blocks(src, 180, &CollapseConfig::uniform(true, 3));
        assert_eq!(result, "\tif (cond) { doSomething(); }\n");
    }

    #[test]
    fn collapse_nested_if_with_obj_lit() {
        // The inner obj lit must collapse first, then the outer if-block
        let src = "\tif (items[activeIndex]) {\n\t\titems[activeIndex].scrollIntoView({\n\t\t\tblock: \"nearest\"\n\t\t});\n\t}\n";
        let result = collapse_single_stmt_blocks(src, 180, &CollapseConfig::uniform(true, 3));
        assert_eq!(
            result,
            "\tif (items[activeIndex]) { items[activeIndex].scrollIntoView({ block: \"nearest\" }); }\n"
        );
    }

    #[test]
    fn else_if_chain_stays_expanded() {
        let src = "\t} else if (e.key === \"Escape\") {\n\t\tlist.classList.add(\"hidden\");\n\t}\n";
        let result = collapse_single_stmt_blocks(src, 180, &CollapseConfig::uniform(true, 3));
        assert_eq!(result, src);
    }

    #[test]
    fn collapse_obj_lit_param() {
        let src = "\t\titems[activeIndex].scrollIntoView({\n\t\t\tblock: \"nearest\"\n\t\t});\n";
        let result = collapse_single_stmt_blocks(src, 180, &CollapseConfig::uniform(true, 3));
        assert_eq!(result, "\t\titems[activeIndex].scrollIntoView({ block: \"nearest\" });\n");
    }

    #[test]
    fn collapse_triple_nested_blocks() {
        let src = "if (a) {\n\tif (b) {\n\t\tif (c) {\n\t\t\tstmt;\n\t\t}\n\t}\n}\n";
        let result = collapse_single_stmt_blocks(src, 180, &CollapseConfig::uniform(true, 3));
        assert_eq!(result, "if (a) { if (b) { if (c) { stmt; } } }\n");
    }

    #[test]
    fn collapse_too_wide_stays_multi_line() {
        // The collapsed line would exceed 40 chars, so it stays multi-line
        let src = "if (reallyLongConditionName) {\n\treallyLongFunctionCall(withArgs);\n}\n";
        let result = collapse_single_stmt_blocks(src, 40, &CollapseConfig::uniform(true, 3));
        assert_eq!(result, src);
    }

    #[test]
    fn collapse_already_collapsed_is_idempotent() {
        let src = "if (cond) { doSomething(); }\n";
        let result = collapse_single_stmt_blocks(src, 180, &CollapseConfig::uniform(true, 3));
        assert_eq!(result, src);
    }

    #[test]
    fn collapse_skips_single_line_comment_body() {
        // A block whose only content is a `//` comment must NOT be collapsed:
        // collapsing would make the closing `}` part of the comment, breaking JS parsing.
        let src = "} catch (e) {\n\t// Silent fail\n}\n";
        let result = collapse_single_stmt_blocks(src, 180, &CollapseConfig::uniform(true, 3));
        // Must stay multi-line — the comment body cannot be safely collapsed
        assert_eq!(result, src);
    }

    // ─── fix_arrow_spacing tests ────────────────────────────────

    #[test]
    fn arrow_spacing_parens() {
        // `()=>{` → `() => {`
        assert_eq!(fix_arrow_spacing("()=>{"), "() => {");
    }

    #[test]
    fn arrow_spacing_with_param() {
        // `(x)=>` → `(x) => `
        assert_eq!(fix_arrow_spacing("(x)=>x"), "(x) => x");
    }

    #[test]
    fn arrow_spacing_single_param_no_parens() {
        // `x=>` → `x => ` — only arrow spacing is affected, not operators
        assert_eq!(fix_arrow_spacing("x=>x+1"), "x => x+1");
    }

    #[test]
    fn arrow_spacing_async() {
        assert_eq!(fix_arrow_spacing("async ()=>{"), "async () => {");
    }

    #[test]
    fn arrow_spacing_comparison_untouched() {
        // `<=` and `>=` should NOT be modified
        assert_eq!(fix_arrow_spacing("a <= b"), "a <= b");
        assert_eq!(fix_arrow_spacing("a >= b"), "a >= b");
    }

    #[test]
    fn arrow_spacing_no_change_when_already_spaced() {
        assert_eq!(fix_arrow_spacing("() => {"), "() => {");
        assert_eq!(fix_arrow_spacing("(x) => x"), "(x) => x");
    }

    #[test]
    fn arrow_spacing_mixed_code() {
        let input = "const fn = ()=>{\n\treturn a <= b;\n}\n";
        let expected = "const fn = () => {\n\treturn a <= b;\n}\n";
        assert_eq!(fix_arrow_spacing(input), expected);
    }

    #[test]
    fn arrow_spacing_implicit_return() {
        // `(x)=>(` should become `(x) => (`
        assert_eq!(fix_arrow_spacing("(x)=>({x})"), "(x) => ({x})");
    }

    #[test]
    fn collapse_no_false_positive_do_while() {
        // `do {` ends with `{` but before brace is `o`, not `)`
        let src = "do {\n\tstmt;\n} while (cond);\n";
        let result = collapse_single_stmt_blocks(src, 180, &CollapseConfig::uniform(true, 3));
        assert_eq!(result, src);
    }

    #[test]
    fn collapse_consecutive_blocks() {
        let src = "if (a) {\n\tfa();\n}\nif (b) {\n\tfb();\n}\n";
        let result = collapse_single_stmt_blocks(src, 180, &CollapseConfig::uniform(true, 3));
        assert_eq!(result, "if (a) { fa(); }\nif (b) { fb(); }\n");
    }

    #[test]
    fn collapse_empty_body_not_touched() {
        let src = "if (cond) {\n\t\n}\n";
        let result = collapse_single_stmt_blocks(src, 180, &CollapseConfig::uniform(true, 3));
        // Empty body should stay as-is (stmt_trimmed.is_empty() check)
        assert_eq!(result, src);
    }

    #[test]
    fn for_loop_block_stays_expanded() {
        let src = "for (;;) {\n\tstmt();\n}\n";
        let result = collapse_single_stmt_blocks(src, 180, &CollapseConfig::uniform(true, 3));
        assert_eq!(result, src);
    }

    #[test]
    fn while_loop_block_stays_expanded() {
        let src = "while (cond) {\n\tstmt();\n}\n";
        let result = collapse_single_stmt_blocks(src, 180, &CollapseConfig::uniform(true, 3));
        assert_eq!(result, src);
    }

    #[test]
    fn collapse_no_trailing_newline_in_input() {
        // Input without trailing newline
        let src = "if (cond) {\n\tdoIt();\n}";
        let result = collapse_single_stmt_blocks(src, 180, &CollapseConfig::uniform(true, 3));
        assert_eq!(result, "if (cond) { doIt(); }",
            "Should preserve absence of trailing newline");
    }

    #[test]
    fn collapse_obj_lit_with_shorthand_prop() {
        // Object literal with shorthand property `id` should collapse
        let src = "sql_log({\n\ts: \"Delete\",\n\tt: \"text\",\n\tid\n}, req);\n";
        let result = collapse_single_stmt_blocks(src, 180, &any_kv(3));
        assert_eq!(result, "sql_log({ s: \"Delete\", t: \"text\", id }, req);\n");
    }

    #[test]
    fn collapse_obj_lit_with_shorthand_and_template() {
        // Object literal with shorthand `id` AND template literal `${feature}`
        let src = "sql_log({\n\ts: \"Delete\",\n\tt: `${feature}`,\n\tid\n}, req);\n";
        let result = collapse_single_stmt_blocks(src, 180, &any_kv(3));
        assert_eq!(result, "sql_log({ s: \"Delete\", t: `${feature}`, id }, req);\n");
    }

    #[test]
    fn collapse_obj_lit_multi_member() {
        let src = "foo({\n\tx: 1,\n\ty: 2,\n\tz: 3\n});\n";
        let result = collapse_single_stmt_blocks(src, 180, &any_kv(3));
        assert_eq!(result, "foo({ x: 1, y: 2, z: 3 });\n");
    }

    #[test]
    fn collapse_spaced_obj_lit_opener() {
        // Space between function name and `({`
        let src = "foo ({\n\tkey: val\n});\n";
        let result = collapse_single_stmt_blocks(src, 180, &CollapseConfig::uniform(true, 3));
        assert_eq!(result, "foo ({ key: val });\n");
    }

    #[test]
    fn collapse_obj_lit_as_second_arg() {
        // Object literal as non-first argument like dispatchEvent("click", { ... })
        let src = "\t\thidden.dispatchEvent(new Event(\"input\", {\n\t\t\tbubbles: true\n\t\t}));\n";
        let result = collapse_single_stmt_blocks(src, 180, &CollapseConfig::uniform(true, 3));
        assert_eq!(result, "\t\thidden.dispatchEvent(new Event(\"input\", { bubbles: true }));\n");
    }

    #[test]
    fn collapse_obj_lit_as_second_arg_narrow() {
        // Same as above but at a narrow width where it just fits
        let src = "\t\thidden.dispatchEvent(new Event(\"input\", {\n\t\t\tbubbles: true\n\t\t}));\n";
        let result = collapse_single_stmt_blocks(src, 62, &CollapseConfig::uniform(true, 3));
        assert_eq!(result, "\t\thidden.dispatchEvent(new Event(\"input\", { bubbles: true }));\n");
    }

    #[test]
    fn collapse_obj_lit_as_second_arg_too_wide() {
        // Collapsed line is 62 chars, so at width 61 it should NOT collapse
        let src = "\t\thidden.dispatchEvent(new Event(\"input\", {\n\t\t\tbubbles: true\n\t\t}));\n";
        let result = collapse_single_stmt_blocks(src, 61, &CollapseConfig::uniform(true, 3));
        assert_eq!(result, src);
    }

    #[test]
    fn collapse_nested_too_wide_preserves_inner() {
        // Outer if fits, but inner obj lit is too wide for 50 chars
        // Inner should stay expanded since it doesn't fit
        let src = "\tif (cond) {\n\t\tfn(veryLongFunctionName, extremelyLongArgumentThatExceedsFiftyCharacters);\n\t}\n";
        let result = collapse_single_stmt_blocks(src, 50, &CollapseConfig::uniform(true, 3));
        assert_eq!(result, src, "Should stay multi-line if collapsed line exceeds max_width");
    }

    #[test]
    fn collapse_array_simple() {
        // Simple array literal should collapse
        let src = "\tconst arr = [\n\t\t\"vipsheader\",\n\t\tsafe_path\n\t];\n";
        let result = collapse_single_stmt_blocks(src, 180, &CollapseConfig::uniform(true, 3));
        assert_eq!(
            result,
            "\tconst arr = [\"vipsheader\", safe_path];\n"
        );
    }

    #[test]
    fn collapse_array_already_collapsed() {
        let src = "\tconst arr = [\"a\", \"b\"];\n";
        let result = collapse_single_stmt_blocks(src, 180, &CollapseConfig::uniform(true, 3));
        assert_eq!(result, src);
    }

    #[test]
    fn collapse_array_too_wide() {
        let src = "\tconst arr = [\n\t\tone,\n\t\ttwo\n\t];\n";
        // "\tconst arr = [one, two];" = 24 chars, so at width 23 it should NOT collapse
        let result = collapse_single_stmt_blocks(src, 23, &CollapseConfig::uniform(true, 3));
        assert_eq!(result, src);
    }

    #[test]
    fn collapse_array_with_obj_lit_after() {
        // Both the array and the obj literal should collapse
        let src = "\tconst proc = Bun.spawn([\n\t\t\"vipsheader\",\n\t\tsafe_path\n\t], {\n\t\twindowsHide: true\n\t});\n";
        let result = collapse_single_stmt_blocks(src, 180, &CollapseConfig::uniform(true, 3));
        assert_eq!(
            result,
            "\tconst proc = Bun.spawn([\"vipsheader\", safe_path], { windowsHide: true });\n"
        );
    }

    #[test]
    fn collapse_inline_type_literal_in_param_with_swc_like_output() {
        // SWC may expand BOTH the filter_clauses type literal and the return type.
        // collapse_inline_type_literals must collapse both via iterative scanning.
        let swc_like = "export async function search_records(search: string = \"\", offset: number = 0, limit: number = 20, order_by: string = \"id::asc\", scope_clause: string = \"\", filter_clauses: {\n\tclause: string;\n\tparams: any[];\n}[] = []): Promise<{\n\trecords: Record[];\n\ttotal: number;\n}> {\n\ttry {\n\t\treturn { records: [], total: 0 };\n\t} catch (error) {\n\t\tconsole.error(\"Error:\", error);\n\t\treturn { records: [], total: 0 };\n\t}\n}\n";
        let result = collapse_inline_type_literals(swc_like, 180, 3);
        // Both type literals should be collapsed
        assert!(
            result.contains("{ records: Record[]; total: number; }"),
            "Return type should be collapsed: got {:?}",
            result
        );
        assert!(
            result.contains("{ clause: string; params: any[]; }"),
            "filter_clauses type literal should be collapsed: got {:?}",
            result
        );
    }

    #[test]
    fn collapse_single_stmt_does_not_expand_return_type() {
        // After collapse_inline_type_literals, the output should not be re-expanded
        // by collapse_single_stmt_blocks
        let after_type_collapse = "export async function search_records(search: string = \"\", offset: number = 0, limit: number = 20, order_by: string = \"id::asc\", scope_clause: string = \"\", filter_clauses: { clause: string; params: any[]; }[] = []): Promise<{ records: Record[]; total: number; }> {\n\ttry {\n\t\treturn { records: [], total: 0 };\n\t} catch (error) {\n\t\tconsole.error(\"Error:\", error);\n\t\treturn { records: [], total: 0 };\n\t}\n}\n";
        let result = collapse_single_stmt_blocks(after_type_collapse, 180, &CollapseConfig::uniform(true, 3));
        assert!(
            result.contains("{ records: Record[]; total: number; }"),
            "collapse_single_stmt_blocks should NOT expand inline type literals: got {:?}",
            result
        );
        let result2 = collapse_single_stmt_blocks(after_type_collapse, 180, &CollapseConfig::uniform(true, 3));
        assert_eq!(result, result2, "Should be idempotent");
    }

    #[test]
    fn full_pipeline_collapses_type_literal_in_param() {
        // Full pipeline test: multi-param function with inline type literal
        let input = "export async function search_records(\n\tsearch: string = \"\",\n\toffset: number = 0,\n\tlimit: number = 20,\n\torder_by: string = \"id::asc\",\n\tscope_clause: string = \"\",\n\tfilter_clauses: { clause: string; params: any[]; }[] = [],\n): Promise<{ records: Record[]; total: number; }> {\n\ttry {\n\t\treturn { records: [], total: 0 };\n\t} catch (error) {\n\t\tconsole.error(\"Error:\", error);\n\t\treturn { records: [], total: 0 };\n\t}\n}\n";
        let result = format_code_content(input, "ts", 180, CollapseConfig::uniform(true, 3), false);
        // The type literal { clause: string; params: any[]; } should be collapsed to one line
        assert!(
            result.contains("{ clause: string; params: any[]; }"),
            "Inline type literal should be collapsed in full pipeline: got {:?}",
            result
        );
        // The return type { records: Record[]; total: number; } should be collapsed too
        assert!(
            result.contains("{ records: Record[]; total: number; }"),
            "Return type literal should also be collapsed. Got: {:?}",
            result
        );
        // Verify idempotency
        let pass2 = format_code_content(&result, "ts", 180, CollapseConfig::uniform(true, 3), false);
        assert_eq!(result, pass2, "Full pipeline output should be idempotent");
    }

    #[test]
    fn collapse_inline_type_literal_in_param() {
        // Regression: inline type literal inside a function parameter like
        // filter_clauses: { clause: string; params: any[]; }[] = [] should be
        // collapsed back to single line after SWC expands it.
        let src = "\tfilter_clauses: {\n\t\tclause: string;\n\t\tparams: any[];\n\t}[] = []): Promise<{ records: Record[]; total: number; }> {\n";
        let result = collapse_inline_type_literals(src, 180, 3);
        assert_eq!(
            result,
            "\tfilter_clauses: { clause: string; params: any[]; }[] = []): Promise<{ records: Record[]; total: number; }> {\n",
            "Inline type literal in function param should be collapsed: got {:?}",
            result
        );
    }

    #[test]
    fn collapse_standalone_return_obj_lit() {
        // `return { width, height }` should collapse. Then the function body becomes
        // a single statement, so the entire `function foo() { return { ... }; }` collapses.
        let src = "\tfunction foo() {\n\t\treturn {\n\t\t\twidth,\n\t\t\theight\n\t\t};\n\t}\n";
        let result = collapse_single_stmt_blocks(src, 180, &CollapseConfig::uniform(true, 3));
        assert_eq!(
            result,
            "\tfunction foo() { return { width, height }; }\n"
        );
    }

    #[test]
    fn collapse_standalone_const_obj_lit() {
        // `const x = { a: 1, b: 2 }` should collapse when it fits.
        let src = "\tconst x = {\n\t\ta: 1,\n\t\tb: 2\n\t};\n";
        let result = collapse_single_stmt_blocks(src, 180, &any_kv(3));
        assert_eq!(
            result,
            "\tconst x = { a: 1, b: 2 };\n"
        );
    }

    #[test]
    fn collapse_standalone_obj_too_wide_stays_multi() {
        // Should stay multi-line when collapsed line exceeds max_width
        let src = "\treturn {\n\t\twidth,\n\t\theight\n\t};\n";
        // "\treturn { width, height };" = 26 chars, so at width 25 it should NOT collapse
        let result = collapse_single_stmt_blocks(src, 25, &CollapseConfig::uniform(true, 3));
        assert_eq!(result, src);
    }

    #[test]
    fn collapse_standalone_obj_with_spread() {
        // Object literal with spread should collapse
        let src = "\treturn {\n\t\t...obj,\n\t\tkey: val\n\t};\n";
        let result = collapse_single_stmt_blocks(src, 180, &CollapseConfig::uniform(true, 3));
        assert_eq!(
            result,
            "\treturn { ...obj, key: val };\n"
        );
    }

    #[test]
    fn collapse_obj_with_comment_lines_stays_multiline() {
        // Standalone object with embedded // comment lines must never collapse —
        // the comment would comment out the closing `}`, producing invalid TS.
        let src = "export const routes = {\n\t...build_routes(route_defs),\n\t...auth_crud,\n\t// GENERATED CHILD CRUD:start\n\t// GENERATED CHILD CRUD:end\n};\n";
        let result = collapse_single_stmt_blocks(src, 180, &CollapseConfig::uniform(true, 10));
        assert_eq!(result, src, "Object with embedded comments must stay multi-line");
    }

    #[test]
    fn collapse_obj_arg_with_comment_line_stays_multiline() {
        // Object passed as a function argument with an embedded // comment must not collapse.
        let src = "someFunc({\n\tkey: \"val\",\n\t// generated\n});\n";
        let result = collapse_single_stmt_blocks(src, 180, &CollapseConfig::uniform(true, 10));
        assert_eq!(result, src, "Obj-arg with embedded comment must stay multi-line");
    }

    #[test]
    fn collapse_obj_with_trailing_inline_comment_stays_multiline() {
        // A member with a trailing // comment must not collapse — the comment
        // would comment out the rest of the object when inlined.
        let src = "const x = {\n\tkey: val, // intentional\n\tother: val\n};\n";
        let result = collapse_single_stmt_blocks(src, 180, &CollapseConfig::uniform(true, 10));
        assert_eq!(result, src, "Object with trailing inline comment must stay multi-line");
    }

    #[test]
    fn collapse_obj_arg_with_trailing_inline_comment_stays_multiline() {
        // Function-arg object with a trailing // comment on a member must not collapse.
        let src = "fn({\n\tkey: val, // note\n\tother: val\n});\n";
        let result = collapse_single_stmt_blocks(src, 180, &CollapseConfig::uniform(true, 10));
        assert_eq!(result, src, "Obj-arg with trailing inline comment must stay multi-line");
    }

    #[test]
    fn collapse_array_with_comment_lines_stays_multiline() {
        // Array with embedded // comments must never collapse.
        let src = "const arr = [\n\tfoo,\n\tbar,\n\t// generated\n];\n";
        let result = collapse_single_stmt_blocks(src, 180, &CollapseConfig::uniform(true, 10));
        assert_eq!(result, src, "Array with embedded comments must stay multi-line");
    }

    #[test]
    fn collapse_array_with_trailing_inline_comment_stays_multiline() {
        // Array element with a trailing // comment must not collapse.
        let src = "const arr = [\n\tfoo, // keep\n\tbar\n];\n";
        let result = collapse_single_stmt_blocks(src, 180, &CollapseConfig::uniform(true, 10));
        assert_eq!(result, src, "Array with trailing inline comment must stay multi-line");
    }

    #[test]
    fn collapse_obj_without_comments_still_collapses() {
        // Sanity check: objects with no comments should still collapse normally.
        let src = "const x = {\n\t...a,\n\t...b,\n};\n";
        let result = collapse_single_stmt_blocks(src, 180, &CollapseConfig::uniform(true, 10));
        assert_eq!(result, "const x = { ...a, ...b };\n");
    }

    // ─── Narrow wrapWidth boundary tests ──────────────────────────

    #[test]
    fn narrow_width_blocks_that_barely_fit() {
        // Short if-block that just fits in 40 chars
        // "if (cond) { doIt(); }" = 22 chars, with prefix = 23
        let src = "\tif (cond) {\n\t\tdoIt();\n\t}\n";
        let result = collapse_single_stmt_blocks(src, 23, &CollapseConfig::uniform(true, 3));
        assert_eq!(result, "\tif (cond) { doIt(); }\n");
    }

    #[test]
    fn narrow_width_block_one_char_too_wide() {
        // "\tif (cond) { doIt(); }" = 22 chars (tab + 21), so at width=21 it should NOT collapse
        let src = "\tif (cond) {\n\t\tdoIt();\n\t}\n";
        let result = collapse_single_stmt_blocks(src, 21, &CollapseConfig::uniform(true, 3));
        assert_eq!(result, src, "Should stay multi-line when collapsed line is 1 char too wide");
    }

    #[test]
    fn narrow_width_demo_if_block_stays_expanded() {
        // The exact pattern from demo.ree: "if (items[activeIndex]) { items[activeIndex].scrollIntoView({ block: "nearest" }); }"
        // This is ~93 chars + tabs. At width=80, it should NOT collapse.
        let expected_collapsed_len = "\t\t\t\tif (items[activeIndex]) { items[activeIndex].scrollIntoView({ block: \"nearest\" }); }".len();
        assert!(expected_collapsed_len > 80, "collapsed line should be >80 chars for this test to be meaningful");

        let src = "\t\t\t\tif (items[activeIndex]) {\n\t\t\t\t\titems[activeIndex].scrollIntoView({\n\t\t\t\t\t\tblock: \"nearest\"\n\t\t\t\t\t});\n\t\t\t\t}\n";
        let result = collapse_single_stmt_blocks(src, 80, &CollapseConfig::uniform(true, 3));
        // Inner obj lit might still collapse since it's short: "items[activeIndex].scrollIntoView({ block: "nearest" });"
        // That's ~62 chars + 5 tabs = ~67 chars, which fits in 80.
        // But the outer if should NOT collapse because the full line is ~93+ chars
        assert!(
            result.contains("scrollIntoView({ block: \"nearest\" });"),
            "Inner obj lit should collapse even at narrow width"
        );
        assert!(
            !result.contains("if (items[activeIndex]) { items[activeIndex]"),
            "Outer if-block should NOT collapse at width=80"
        );
    }

    #[test]
    fn narrow_width_obj_lit_barely_fits() {
        // "\t\tfoo({ x: 1, y: 2 });" = 22 chars (2 tabs + 20)
        let src = "\t\tfoo({\n\t\t\tx: 1,\n\t\t\ty: 2\n\t\t});\n";
        let result = collapse_single_stmt_blocks(src, 22, &any_kv(3));
        assert_eq!(result, "\t\tfoo({ x: 1, y: 2 });\n");
    }

    #[test]
    fn narrow_width_obj_lit_too_wide() {
        // "\t\tfoo({ x: 1, y: 2 });" = 22 chars, so at width=21 it should NOT collapse
        let src = "\t\tfoo({\n\t\t\tx: 1,\n\t\t\ty: 2\n\t\t});\n";
        let result = collapse_single_stmt_blocks(src, 21, &CollapseConfig::uniform(true, 3));
        assert_eq!(result, src, "Obj lit should stay multi-line when 1 char too wide");
    }

    #[test]
    fn narrow_width_else_if_stays_expanded() {
        let src = "\t} else if (k) {\n\t\tstmt();\n\t}\n";
        let result = collapse_single_stmt_blocks(src, 28, &CollapseConfig::uniform(true, 3));
        assert_eq!(result, src);
    }

    #[test]
    fn narrow_width_for_loop_stays_expanded() {
        let src = "\tfor (;;) {\n\t\tstmt();\n\t}\n";
        let result = collapse_single_stmt_blocks(src, 22, &CollapseConfig::uniform(true, 3));
        assert_eq!(result, src);
    }

    #[test]
    fn narrow_width_while_stays_expanded() {
        let src = "\twhile (c) {\n\t\tstmt();\n\t}\n";
        let result = collapse_single_stmt_blocks(src, 24, &CollapseConfig::uniform(true, 3));
        assert_eq!(result, src);
    }

    // ─── Template literal formatting correctness tests ────────────

    #[test]
    fn ts_with_template_literal_preserves_tabs() {
        // Regression: TS code containing template literals with tab-indented
        // content should preserve the template literal tabs AND correctly
        // indent the surrounding code with single tabs (not 4x tabs).
        let src = "export function foo() {\n\tconst x = `\n\t\t<p>text</p>\n\t`;\n}\n";
        let result = format_code_content(src, "ts", 180, CollapseConfig::uniform(true, 3), false);
        // Code should use single-tab indentation
        assert!(
            result.contains("\tconst x ="),
            "Function body should use single tab, got: {:?}",
            result
        );
        // Template literal HTML should keep its tab indentation
        assert!(
            result.contains("\t\t<p>text</p>"),
            "Template literal content with tabs should be preserved"
        );
        // Closing backtick should keep its tab
        assert!(
            result.contains("\t`;"),
            "Closing backtick should have tab indentation"
        );
    }

    #[test]
    fn ts_with_template_literal_idempotent() {
        // Formatting a TS file with template literals must be idempotent.
        let src = "export function foo() {\n\tconst x = `\n\t\t<p>text</p>\n\t`;\n}\n";
        let pass1 = format_code_content(src, "ts", 180, CollapseConfig::uniform(true, 3), false);
        let pass2 = format_code_content(&pass1, "ts", 180, CollapseConfig::uniform(true, 3), false);
        assert_eq!(
            pass1, pass2,
            "format_code_content with template literals should be idempotent"
        );
    }

    #[test]
    fn ts_with_template_literal_no_excess_indent() {
        // Regression: the function body should NOT be indented with 4x tabs.
        // Each line at body level should have exactly 1 tab.
        let src = "export function foo() {\n\tconst x = `\n\t\t<p>text</p>\n\t`;\n}\n";
        let result = format_code_content(src, "ts", 180, CollapseConfig::uniform(true, 3), false);
        // Check that the result doesn't have 4 tabs at body level (old bug)
        assert!(
            !result.contains("\t\t\t\tconst x ="),
            "Should NOT have 4 tabs at function body level: got {:?}",
            result
        );
        // Check that function body lines have exactly 1 tab, not more
        for line in result.lines() {
            if line.contains("const x") {
                let trimmed = line.trim_start();
                let leading = line.len() - trimmed.len();
                assert_eq!(
                    leading, 1,
                    "Function body line should have exactly 1 tab, got {} leading chars",
                    leading
                );
            }
        }
    }

    // ─── wrap_long_method_chains tests ─────────────────────────

    #[test]
    fn wrap_method_chain_simple_template() {
        // Method chain inside a template literal — the user's original example
        // Has 4 `.method()` calls: .entries, .filter, .map, .join
        // First call (.entries) stays on first line; .filter, .map, .join get split
        let src = "const grid_cols = `${Object.entries(columns).filter(([_, v]: [string, any]) => v.grid !== false).map(([_, v]: [string, any]) => (typeof v === \"string\" ? v : v.width)).join(\" \")} auto`;\n";
        assert!(src.len() > 180, "line should exceed 180 chars, got {} chars", src.len());
        let result = wrap_long_method_chains(src, 180);
        assert!(
            result.contains("Object.entries(columns)\n"),
            "Object.entries(columns) should be the end of first line: got {:?}",
            result
        );
        assert!(
            result.contains("\t.filter(([_, v]: [string, any]) => v.grid !== false)\n"),
            ".filter should be on its own indented line"
        );
        assert!(
            result.contains("\t.map(([_, v]: [string, any]) => (typeof v === \"string\" ? v : v.width))\n"),
            ".map should be on its own indented line"
        );
        assert!(
            result.contains("\t.join(\" \")} auto`;"),
            "Last .join + rest on its own indented line"
        );
    }

    #[test]
    fn wrap_method_chain_fits_width_no_split() {
        // Method chain that fits within max_width — should NOT split
        let src = "const result = arr.filter(fn).map(fn2).join(\",\");\n";
        let result = wrap_long_method_chains(src, 180);
        assert_eq!(result, src, "Should not split when line fits within max_width");
    }

    #[test]
    fn wrap_method_chain_single_call_no_split() {
        // Only one `.method()` call — not a chain, should NOT split
        let src = "const longPrefix = someExpression.filter(x => x > 0 && x < 100 && x !== null && x !== undefined);\n";
        // Line is ~107 chars
        let result = wrap_long_method_chains(src, 97);
        assert_eq!(result, src, "Single method call should not split");
    }

    #[test]
    fn wrap_method_chain_two_calls_no_split() {
        // Two method calls — only 2 < 3, so should NOT split
        let src = "const result = someBase.filter(x => x > 0 && x < 100 && x.isValid()).map(y => y.toString());\n";
        assert!(src.len() > 80, "line should exceed 80 chars, got {} chars", src.len());
        let result = wrap_long_method_chains(src, 80);
        assert_eq!(result, src, "Two method calls (< 3) should not split");
    }

    #[test]
    fn wrap_method_chain_already_split_idempotent() {
        // Already-split method chain should be idempotent
        let src = "const grid_cols = `${Object.entries(columns)\n\t.filter(([_, v]: [string, any]) => v.grid !== false)\n\t.map(([_, v]: [string, any]) => (typeof v === \"string\" ? v : v.width))\n\t.join(\" \")} auto`;\n";
        let result = wrap_long_method_chains(src, 180);
        assert_eq!(result, src, "Already-split method chain should be idempotent");
    }

    #[test]
    fn wrap_method_chain_no_false_positive_dot_in_string() {
        // `.` inside a string should not trigger
        let src = "const msg = \"hello.world\";\n";
        let result = wrap_long_method_chains(src, 40);
        assert_eq!(result, src);
    }

    #[test]
    fn wrap_method_chain_three_calls_split() {
        // Three method calls exceeding width
        let src = "const result = base.filter(x => x > 0 && x < 100 && x.isValid()).map(y => y.toString()).join(\",\");\n";
        assert_eq!(src.len(), 99, "line is 99 chars");
        let result = wrap_long_method_chains(src, 97);
        assert!(!result.contains(src.trim_end()), "Should not contain original line");
        assert!(result.contains("base.filter(x => x > 0 && x < 100 && x.isValid())\n"), "First call stays on first line");
        assert!(result.contains("\t.map(y => y.toString())\n"), "Second call on its own indented line");
        assert!(result.contains("\t.join(\",\");"), "Last call + rest on last indented line");
    }

    #[test]
    fn wrap_method_chain_numeric_dot_ignored() {
        // Numeric `.` like 3.14 should NOT be treated as method call
        // Only 2 calls (.toFixed, .valueOf) after the numeric 3.14 — < 3, so no split
        let src = "const val = 3.14.toFixed(2).valueOf();\n";
        let result = wrap_long_method_chains(src, 40);
        assert_eq!(result, src, "Two method calls after numeric literal should not split (need >= 3)");
    }

    #[test]
    fn wrap_method_chain_full_pipeline() {
        // Full pipeline test: the method chain inside template literal
        let input = "const grid_cols = `${Object.entries(columns).filter(([_, v]: [string, any]) => v.grid !== false).map(([_, v]: [string, any]) => (typeof v === \"string\" ? v : v.width)).join(\" \")} auto`;\n";
        let result = format_code_content(input, "ts", 180, CollapseConfig::uniform(true, 3), false);
        // The method chain should be split
        assert!(result.contains("Object.entries(columns)\n"), "Method chain should be split");
        assert!(result.contains(".filter("), ".filter should be present");
        assert!(result.contains(".map("), ".map should be present");
        assert!(result.contains(".join(\" \")}"), ".join should be present");
        // Verify idempotency
        let pass2 = format_code_content(&result, "ts", 180, CollapseConfig::uniform(true, 3), false);
        assert_eq!(result, pass2, "Full pipeline should be idempotent");
    }

    #[test]
    fn wrap_method_chain_no_false_positive_or_operator() {
        // Regression: `a.b().c() || x.d() || x.e()` must NOT be chain-split.
        // The || separates independent chains, so no single contiguous chain has ≥ 3 calls.
        let src = "\t\tconst is_sqlite = conn_str.toLowerCase().startsWith(\"sqlite://\") || conn_str.endsWith(\".sqlite\") || conn_str.endsWith(\".db\");\n";
        assert!(src.len() > 100);
        let result = wrap_long_method_chains(src, 100);
        // Must be unchanged — splitting would corrupt semantics
        assert_eq!(result, src, "Line with || between chains must not be split: got {:?}", result);
    }

    // ─── wrap_long_function_params tests ────────────────────────

    #[test]
    fn wrap_function_params_no_false_positive_template_literal() {
        // Regression: VALUES (${a}, ${"2026-01-01 00:00:00"}) inside a tagged
        // template must not be treated as a function call and split.
        // The colon in the date string previously satisfied the type-annotation
        // guard, causing the template content to be corrupted.
        let src = "async function seed_user(username: string, email: string) {\n\tawait test_db!`\n\t\tINSERT INTO users (email, name)\n\t\tVALUES (${email}, ${\"Seed\"}, ${\"\"}, ${username}, ${\"\"}, ${\"\"}, ${modules}, ${\"2026-01-01 00:00:00\"})\n\t`;\n}\n";
        let result = wrap_long_function_params(src, 100, 3);
        assert_eq!(result, src, "Template literal content must not be split as function params");
    }

    #[test]
    fn wrap_function_params_no_false_positive_block_comment() {
        // Regression: content inside a multi-line block comment must not be
        // treated as function params. The `:` in backtick literals like
        // `bun build:dist` previously satisfied the type-annotation guard.
        let src = "/**\n * pipeline (`bun build:dist`, `bun preview`, sitemap, rss) works against it.\n */\nexport const X = 1;\n";
        let result = wrap_long_function_params(src, 80, 3);
        assert_eq!(result, src, "Block comment content must not be split as function params");
    }

    #[test]
    fn wrap_split_function_params_6_params_exceeds_width() {
        // 6 params, line is ~230 chars, wrapWidth=180 → split one-per-line
        let src = "export async function search_records(search: string = \"\", offset: number = 0, limit: number = 20, order_by: string = \"id::asc\", scope_clause: string = \"\", filter_clauses: { clause: string; params: any[]; }[] = []): Promise<{ records: Record[]; total: number; }> {\n";
        let result = wrap_long_function_params(src, 180, 3);
        assert!(result.starts_with("export async function search_records("), "Should keep open paren on first line");
        assert!(result.contains("\n\tsearch: string = \"\""), "First param should be indented one level from column 0");
        assert!(result.contains("\n\tfilter_clauses: { clause: string; params: any[]; }[] = []"), "Last param indented one level");
        assert!(result.contains("\n): Promise<"), ") should be at original indent level (0)");
        // Count total params — each param on its own line followed by a comma
        let param_lines: Vec<&str> = result.lines()
            .filter(|l| l.trim().starts_with("search:") || l.trim().starts_with("offset:") || l.trim().starts_with("limit:") || l.trim().starts_with("order_by:") || l.trim().starts_with("scope_clause:") || l.trim().starts_with("filter_clauses:"))
            .collect();
        assert_eq!(param_lines.len(), 6, "Should have 6 param lines");
    }

    #[test]
    fn wrap_skip_function_3_params() {
        // 3 params, even if line exceeds width, should NOT split
        let src = "export function foo(a: string, b: string, c: string): void {\n";
        let result = wrap_long_function_params(src, 40, 3);
        assert_eq!(result, src);
    }

    #[test]
    fn wrap_skip_function_fits_width() {
        // 4 params but line fits within maxWidth → keep one-line
        let src = "function foo(a: string, b: string, c: string, d: string) {\n";
        assert!(src.len() <= 80, "line should fit within 80 for this test");
        let result = wrap_long_function_params(src, 80, 3);
        assert_eq!(result, src);
    }

    #[test]
    fn wrap_async_function_split() {
        // Async function declaration with many params
        let src = "export async function process_data(id: number, name: string, value: number, options: Record<string, any>, callback: () => void): Promise<void> {\n";
        assert!(src.len() > 120, "line should exceed 120 for this test");
        let result = wrap_long_function_params(src, 120, 3);
        assert!(result.starts_with("export async function process_data("), "Should start with function name + open paren");
        assert!(result.contains("\n\tid: number,"), "Params should be indented one level from column 0");
        assert!(result.contains("\n\tcallback: () => void,"), "Last param should have trailing comma");
        assert!(result.contains("\n): Promise<void>"), "Close paren at original indent (0)");
    }

    #[test]
    fn wrap_idempotent_already_split() {
        // If already formatted with params on separate lines, should not change
        let src = "export async function search_records(\n\tsearch: string = \"\",\n\toffset: number = 0,\n\tlimit: number = 20,\n\torder_by: string = \"id::asc\",\n\tscope_clause: string = \"\",\n\tfilter_clauses: { clause: string; params: any[]; }[] = [],\n): Promise<{ records: Record[]; total: number; }> {\n\ttry {\n\t\treturn { records: [], total: 0 };\n\t} catch (error) {\n\t\tconsole.error(\"Error:\", error);\n\t\treturn { records: [], total: 0 };\n\t}\n}\n";
        let result = wrap_long_function_params(src, 180, 3);
        assert_eq!(result, src, "Already-split function should not be modified");
    }

    #[test]
    fn wrap_no_function_keyword_no_change() {
        // Lines without `function` keyword should pass through unchanged
        let src = "const x = 1;\nconst y = 2;\n";
        let result = wrap_long_function_params(src, 180, 3);
        assert_eq!(result, src);
    }

    #[test]
    fn wrap_full_pipeline_multi_param_function() {
        // Full pipeline: function with params already split should stay split
        let input = "export async function search_records(\n\tsearch: string = \"\",\n\toffset: number = 0,\n\tlimit: number = 20,\n\torder_by: string = \"id::asc\",\n\tscope_clause: string = \"\",\n\tfilter_clauses: { clause: string; params: any[]; }[] = [],\n): Promise<{ records: Record[]; total: number; }> {\n\ttry {\n\t\treturn { records: [], total: 0 };\n\t} catch (error) {\n\t\tconsole.error(\"Error:\", error);\n\t\treturn { records: [], total: 0 };\n\t}\n}\n";
        let result = format_code_content(input, "ts", 180, CollapseConfig::uniform(true, 3), false);
        // Should keep params on separate lines (already split)
        assert!(result.contains("search_records("), "Open paren on first line");
        assert!(result.contains("\n\tsearch: string"), "Params should be on separate lines");
        // Verify idempotency
        let pass2 = format_code_content(&result, "ts", 180, CollapseConfig::uniform(true, 3), false);
        assert_eq!(result, pass2, "Full pipeline should be idempotent");
    }

    #[test]
    fn wrap_class_method_split() {
        // Class method declaration (no `function` keyword)
        // Line is ~63 chars, wrapWidth=50 → should split
        let src = "\t\tgetData(a: string, b: number, c: string, d: boolean): void {\n";
        assert!(src.len() > 50, "line should exceed 50 chars for this test");
        let result = wrap_long_function_params(src, 50, 3);
        assert!(result.contains("getData("), "Should keep method name + open paren on first line");
        assert!(result.contains("\n\t\t\ta: string,"), "First param should be indented one more level");
        assert!(result.contains("\n\t\t\td: boolean,"), "Last param should have trailing comma");
        assert!(result.contains("\n\t\t): void {"), "Close paren at original indent level");
    }

    #[test]
    fn wrap_class_method_async_split() {
        // Async method with 5 params exceeding width
        let src = "\tasync fetchData(id: number, name: string, value: number, options: Record<string, any>, callback: () => void): Promise<void> {\n";
        assert!(src.len() > 120, "line should exceed 120 chars");
        let result = wrap_long_function_params(src, 120, 3);
        assert!(result.starts_with("\tasync fetchData("), "Should keep async + method name");
        assert!(result.contains("\n\t\tid: number,"), "Params indented one more level");
    }

    #[test]
    fn wrap_interface_method_split() {
        // Interface method declaration (no body, just return type)
        let src = "getData(a: string, b: number, c: string, d: boolean): Record[];\n";
        let result = wrap_long_function_params(src, 60, 3);
        assert!(result.starts_with("getData("), "Should keep method name + open paren");
        assert!(result.contains("\n\ta: string,"), "Params indented one level");
        assert!(result.contains("\n): Record[];"), "Close paren at original indent");
    }

    #[test]
    fn wrap_method_call_no_false_positive() {
        // Function call without type annotations should NOT trigger
        let src = "\treturn someFunction(a, b, c, d, e, f);\n";
        let result = wrap_long_function_params(src, 40, 3);
        assert_eq!(result, src, "Function calls without type annotations should not trigger");
    }

    #[test]
    fn wrap_method_3_params_only() {
        // Method with 3 params should NOT split
        let src = "\tgetName(a: string, b: string, c: string): string {\n";
        let result = wrap_long_function_params(src, 40, 3);
        assert_eq!(result, src, "Methods with <=3 params should not split");
    }

    #[test]
    fn wrap_method_fits_width() {
        // Method with 4 params that fits within width should NOT split
        let src = "\tgetShort(a: string, b: string, c: string, d: string) {\n";
        assert!(src.len() <= 80, "line should fit within 80 cols");
        let result = wrap_long_function_params(src, 80, 3);
        assert_eq!(result, src, "Method that fits within max_width should not split");
    }

    #[test]
    fn wrap_arrow_function_split() {
        // Arrow function expression assigned to a const
        let src = "const processData = (a: string, b: number, c: string, d: boolean): void => {\n";
        assert!(src.len() > 70, "line should exceed 70 chars for this test");
        let result = wrap_long_function_params(src, 70, 3);
        assert!(result.starts_with("const processData = ("), "Should keep const + arrow function + open paren");
        assert!(result.contains("\n\ta: string,"), "First param indented");
        assert!(result.contains("\n): void => {"), "Close paren at original indent");
        assert!(result.contains("=> {"), "Arrow function body preserved");
    }

    #[test]
    fn wrap_arrow_async_function_split() {
        // Async arrow function assigned to a const
        let src = "const fetchData = async (id: number, name: string, value: number, options: Record<string, any>, callback: () => void): Promise<void> => {\n";
        assert!(src.len() > 130, "line should exceed 130 chars for this test");
        let result = wrap_long_function_params(src, 130, 3);
        assert!(result.starts_with("const fetchData = async ("), "Should keep const + async + open paren");
        assert!(result.contains("\n\tid: number,"), "Params indented");
        assert!(result.contains("\n): Promise<void> => {"), "Close paren + arrow at original indent");
        // Verify idempotency
        let pass2 = wrap_long_function_params(&result, 150, 3);
        assert_eq!(result, pass2, "Arrow function split should be idempotent");
    }

    #[test]
    fn wrap_arrow_function_3_params_no_split() {
        // Arrow function with 3 params should NOT split
        let src = "const fn = (a: string, b: string, c: string): void => {\n";
        let result = wrap_long_function_params(src, 40, 3);
        assert_eq!(result, src);
    }

    #[test]
    fn wrap_arrow_function_call_no_false_positive() {
        // Regular function call assigned to const (no type annotations) should NOT trigger
        let src = "const result = someFunction(a, b, c, d, e, f);\n";
        let result = wrap_long_function_params(src, 40, 3);
        assert_eq!(result, src, "Function calls without type annotations should not trigger");
    }

    #[test]
    fn wrap_inside_body_no_false_positive() {
        // Function call or method call inside function body should not trigger
        let src = "\treturn someFunction(a, b, c, d, e, f);\n";
        let result = wrap_long_function_params(src, 40, 3);
        assert_eq!(result, src, "Function calls without 'function' keyword should not trigger");
    }

    #[test]
    fn ts_complex_template_literal_preserved() {
        // A more realistic template literal with multiple levels of nesting.
        let src = "export function page() {\n\tconst html = `\n\t\t<div>\n\t\t\t<p>content</p>\n\t\t</div>\n\t`;\n\treturn html;\n}\n";
        let result = format_code_content(src, "ts", 180, CollapseConfig::uniform(true, 3), false);
        // Code indentation: single tab
        assert!(result.contains("\tconst html ="), "Code should use single tab");
        assert!(result.contains("\treturn html;"), "Return should use single tab");
        // Template literal: deep tabs preserved
        assert!(result.contains("\t\t\t<p>content</p>"), "Deep template tabs preserved");
        // Closing brace at column 0
        let lines: Vec<&str> = result.lines().collect();
        assert_eq!(lines.last(), Some(&"}"), "Closing brace should be at column 0");
        // Idempotent
        let pass2 = format_code_content(&result, "ts", 180, CollapseConfig::uniform(true, 3), false);
        assert_eq!(result, pass2, "Complex template literal should be idempotent");
    }

    #[test]
    fn template_body_not_collapsed_after_backtick_in_string() {
        // Regression: a backtick inside a string literal (`quote !== "`"`) must
        // not flip template-literal tracking. Previously it did, leaving a later
        // multi-line template literal unprotected so its inner `if` block was
        // collapsed onto one line — a semantic change to the generated code.
        let src = "function f() {\n\tif (quote !== \"`\") return null;\n\tconst helper = `const h = 1;\nfor (const x of y) {\n  if (typeof x === 'fn') {\n    run(x);\n  }\n}`;\n\treturn helper;\n}\n";
        let result = format_code_content(src, "ts", 150, CollapseConfig::uniform(true, 4), false);
        assert!(
            !result.contains("if (typeof x === 'fn') { run(x); }"),
            "template body must not be collapsed:\n{}",
            result
        );
        assert!(
            result.contains("  if (typeof x === 'fn') {\n    run(x);\n  }"),
            "template body must be preserved verbatim:\n{}",
            result
        );
        let pass2 = format_code_content(&result, "ts", 150, CollapseConfig::uniform(true, 4), false);
        assert_eq!(result, pass2, "should be idempotent");
    }

    #[test]
    fn inline_block_comment_in_empty_catch_preserved() {
        // Regression: an inline block comment inside an empty catch block
        // (`catch { /* dead */ }`) attaches as a trailing comment of the opening
        // brace. The printer previously only looked for line comments before the
        // closing brace, silently dropping it.
        let src = "function f() {\n\ttry {\n\t\tx();\n\t} catch { /* process already dead */ }\n}\n";
        let result = format_code_content(src, "ts", 150, CollapseConfig::uniform(true, 4), false);
        assert!(
            result.contains("/* process already dead */"),
            "block comment in empty catch must be preserved:\n{}",
            result
        );
        let pass2 = format_code_content(&result, "ts", 150, CollapseConfig::uniform(true, 4), false);
        assert_eq!(result, pass2, "should be idempotent");
    }

    #[test]
    fn collapse_array_with_multiline_object_not_flattened() {
        // Array containing a multi-line object must NOT be collapsed to one line —
        // joining `{` as an array member produces `[{, type: "text", ...}]` which
        // is invalid TypeScript.
        let src = "const x = [\n\t{\n\t\ttype: \"text\",\n\t\ttext: `error: ${e.message}`,\n\t},\n];\n";
        let result = format_code_content(src, "ts", 180, CollapseConfig::uniform(true, 4), false);
        assert!(!result.contains("[{,"), "Array with multi-line object must not collapse to [{{, ...}}]");
        let pass2 = format_code_content(&result, "ts", 180, CollapseConfig::uniform(true, 4), false);
        assert_eq!(result, pass2, "Should be idempotent");
    }

    // ─── Soft wrap width (collapse overrides member count) ─────────

    /// Build a collapse config with the soft-width override enabled.
    fn soft(max: usize, soft_width: usize) -> CollapseConfig {
        let mut cfg = CollapseConfig::uniform(true, max);
        cfg.soft_wrap_width = soft_width;
        cfg
    }

    /// uniform() with the key:value-prop limit lifted, for tests that exercise
    /// object-collapse mechanics independent of the named-property rule.
    fn any_kv(max: usize) -> CollapseConfig {
        let mut cfg = CollapseConfig::uniform(true, max);
        cfg.max_keyvalue_props = usize::MAX;
        cfg
    }

    #[test]
    fn soft_width_collapses_call_over_count_cap() {
        // 5 args > cap(4) but inline form is short (~46 chars < soft 100) →
        // collapse onto one line instead of exploding one-per-line.
        let src = "const nested = join(\n\tbase,\n\t\"x\",\n\t\"y\",\n\t\"z\",\n\t\"w\",\n);\n";
        let result = format_code_content(src, "ts", 180, soft(4, 100), false);
        assert_eq!(result, "const nested = join(base, \"x\", \"y\", \"z\", \"w\");\n");
        let pass2 = format_code_content(&result, "ts", 180, soft(4, 100), false);
        assert_eq!(result, pass2, "soft-collapsed call should be idempotent");
    }

    #[test]
    fn soft_width_expands_call_when_over_soft_and_count() {
        // count > cap AND inline width > soft → still explodes one-per-line.
        let src = "const b = makeRequest(longArgumentNumberOne, longArgumentNumberTwo, longArgumentNumberThree, longArgumentNumberFour, longArgumentNumberFive, longArgumentNumberSix);\n";
        let result = format_code_content(src, "ts", 180, soft(4, 100), false);
        assert!(result.contains("makeRequest(\n"), "wide 6-arg call should expand: {result}");
        assert!(result.matches('\n').count() >= 7, "each arg on its own line");
    }

    #[test]
    fn soft_width_keeps_low_count_wide_call_inline() {
        // 3 args <= cap, inline width in the 100..180 band → count path keeps it
        // inline (this is the asymmetry that distinguishes option 1 from a pure
        // width rule).
        let src = "const c = combineThings(firstReasonablyLongArgumentHere, secondReasonablyLongArgument, thirdReasonablyLongOneToo);\n";
        let result = format_code_content(src, "ts", 180, soft(4, 100), false);
        assert_eq!(result.matches('\n').count(), 1, "should stay on one line: {result}");
    }

    #[test]
    fn soft_width_collapses_array_and_object_over_count_cap() {
        let arr = format_code_content(
            "const arr = [\n\t1,\n\t2,\n\t3,\n\t4,\n\t5,\n\t6,\n];\n",
            "ts", 180, soft(4, 100), false,
        );
        assert_eq!(arr, "const arr = [1, 2, 3, 4, 5, 6];\n");

        let obj = format_code_content(
            "const o = {\n\ta: 1,\n\tb: 2,\n\tc: 3,\n\td: 4,\n\te: 5,\n\tf: 6,\n};\n",
            "ts", 180, soft(4, 100), false,
        );
        assert_eq!(obj, "const o = { a: 1, b: 2, c: 3, d: 4, e: 5, f: 6 };\n");
    }

    #[test]
    fn soft_width_zero_preserves_count_cap() {
        // soft = 0 disables the override: 5 short args still explode at cap 4.
        let src = "const nested = join(base, \"x\", \"y\", \"z\", \"w\");\n";
        let result = format_code_content(src, "ts", 180, soft(4, 0), false);
        assert!(result.contains("join(\n"), "soft=0 should keep count-cap behavior: {result}");
    }

    // ─── Tab-aware width measurement ───────────────────────────────

    #[test]
    fn display_width_expands_tabs_to_stops() {
        assert_eq!(display_width("abc", 4), 3);
        assert_eq!(display_width("\tabc", 4), 7); // tab → col 4, then +3
        assert_eq!(display_width("\t\t", 4), 8); // two tab stops
        assert_eq!(display_width("a\tb", 4), 5); // a(1) → tab to 4 → b(5)
        assert_eq!(display_width("\tx", 1), 2); // tab_width 1 == legacy raw count
        assert_eq!(display_width("", 4), 0);
    }

    #[test]
    fn tab_width_decides_deep_object_collapse() {
        // The `name` object has 5 members (> cap 4) and an 88-char inline body.
        // It sits at 4-tab depth. With tab_width=4 the line lands at 16+88=104
        // display columns (> softWidth 100) so it must stay expanded — this is
        // the "tabs push it off screen" case. With tab_width=1 the same line is
        // only 92 columns (<= 100) so the soft override collapses it.
        let src = "function f() {\n\tfunction g() {\n\t\tconst c = setup({\n\t\t\tfields: {\n\t\t\t\tname: {\n\t\t\t\t\tname: \"name\",\n\t\t\t\t\ttype: \"text\",\n\t\t\t\t\trequired: true,\n\t\t\t\t\tis_nullable: false,\n\t\t\t\t\tattributes: {},\n\t\t\t\t},\n\t\t\t},\n\t\t});\n\t}\n}\n";
        let collapsed_marker = "{ name: \"name\", type: \"text\"";

        let mut wide_tab = soft(4, 100);
        wide_tab.tab_width = 4;
        let expanded = format_code_content(src, "ts", 180, wide_tab, false);
        assert!(!expanded.contains(collapsed_marker), "tab=4: 5-member object should stay expanded:\n{expanded}");

        let mut narrow_tab = soft(4, 100);
        narrow_tab.tab_width = 1;
        let collapsed = format_code_content(src, "ts", 180, narrow_tab, false);
        assert!(collapsed.contains(collapsed_marker), "tab=1: same object fits in 92 cols and collapses:\n{collapsed}");
    }
}
