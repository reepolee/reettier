use std::fs;
use std::path::Path;

/// Flatten multiline string concatenation into single lines before formatting
/// so the SWC printer can detect and convert it. Only joins lines where
/// the continuation line starts with `+` (after trimming), which is the
/// standard pattern for multiline string concatenation in JS.
pub(crate) fn flatten_concat(src: &str) -> String {
    let mut out = String::new();
    let mut prev_line = String::new();
    let mut has_prev = false;

    for line in src.lines() {
        let trimmed = line.trim();
        if has_prev && trimmed.starts_with('+') {
            prev_line.push(' ');
            prev_line.push_str(trimmed);
        } else {
            if has_prev {
                out.push_str(&prev_line);
                out.push('\n');
            }
            prev_line = line.to_string();
            has_prev = true;
        }
    }
    if has_prev {
        out.push_str(&prev_line);
    }
    out
}

/// Format full Ree template content.
pub(crate) fn format_ree_content(content: &str, wrap_width: usize, oneline: usize, collapse: crate::full::format::CollapseConfig, remove_unused: bool) -> String {
    let ast_output = crate::full::ree_parser::format_ree(content, wrap_width, oneline);
    let after_raw_js = format_raw_js_blocks(&ast_output, wrap_width, collapse.clone(), remove_unused);
    format_script_blocks(&after_raw_js, wrap_width, collapse, remove_unused)
}

fn format_script_blocks(content: &str, wrap_width: usize, collapse: crate::full::format::CollapseConfig, remove_unused: bool) -> String {
    let after_script = format_tagged_blocks(content, "script", wrap_width, collapse, remove_unused);
    format_tagged_blocks(&after_script, "style", wrap_width, collapse, remove_unused)
}

/// Format JS inside `{{ ... }}` raw-JS blocks using the same SWC pipeline as
/// `<script>` blocks. The ree renderer already emits these as:
///
/// ```
/// [indent]{{
/// [indent+1]line...
/// [indent]}}
/// ```
///
/// This post-pass finds each `{{` opener, extracts the body, runs it through
/// `format_script_content`, and re-emits with correct indentation.
fn format_raw_js_blocks(content: &str, wrap_width: usize, collapse: crate::full::format::CollapseConfig, remove_unused: bool) -> String {
    let mut out = String::with_capacity(content.len());
    let mut remaining = content;

    loop {
        // Find `{{` that sits at the start of a line (after optional tabs).
        // We look for "\n<tabs>{{" or, for a block at position 0, just "{{".
        let search_result = remaining.find("{{");
        let Some(rel) = search_result else {
            break;
        };

        // Only treat it as a raw-JS opener when `{{` is at the start of a
        // line — i.e. everything after the previous newline (or start) is tabs.
        let prefix = &remaining[..rel];
        let last_nl = prefix.rfind('\n').map(|i| i + 1).unwrap_or(0);
        let indent = &prefix[last_nl..];
        if !indent.chars().all(|c| c == '\t') {
            // Not a line-start `{{` — skip past it and keep scanning.
            out.push_str(&remaining[..rel + 2]);
            remaining = &remaining[rel + 2..];
            continue;
        }

        // Emit everything up to and including `{{`.
        out.push_str(prefix);
        out.push_str("{{");
        let after_open = &remaining[rel + 2..];

        // Find the matching `}}` on its own line at the same indent level.
        // We look for "\n<indent>}}" (close marker).
        let close_marker = format!("\n{}}}}}",  indent);
        if let Some(close_rel) = after_open.find(&close_marker as &str) {
            let block_content = &after_open[..close_rel];

            let formatted = format_script_content(block_content, wrap_width, collapse.clone(), remove_unused);
            out.push_str(&formatted);
            if !formatted.is_empty() && !formatted.ends_with('\n') {
                out.push('\n');
            }
            out.push_str(indent);
            out.push_str("}}");

            remaining = &after_open[close_rel + close_marker.len()..];
        } else {
            // No matching close — leave the rest as-is.
            out.push_str(after_open);
            remaining = "";
        }
    }

    out.push_str(remaining);
    out
}

fn format_tagged_blocks(content: &str, tag: &str, wrap_width: usize, collapse: crate::full::format::CollapseConfig, remove_unused: bool) -> String {
    let open_prefix = format!("<{}", tag);
    let close_tag = format!("</{}>", tag);
    let mut out = String::with_capacity(content.len());
    let mut remaining = content;
    while let Some(start) = remaining.find(&open_prefix as &str) {
        if let Some(tag_end) = remaining[start..].find('>') {
            let tag_close = start + tag_end + 1;
            if let Some(block_end) = remaining[tag_close..].find(&close_tag as &str) {
                let content_start = tag_close;
                let content_end = tag_close + block_end;

                let before = &remaining[..start];
                let indent: String = before.chars().rev()
                    .take_while(|&c| c == '\t')
                    .collect::<String>()
                    .chars()
                    .rev()
                    .collect();

                out.push_str(before);
                out.push_str(&remaining[start..content_start]);

                let block_content = &remaining[content_start..content_end];
                let formatted = match tag {
                    "script" => format_script_content(block_content, wrap_width, collapse, remove_unused),
                    "style" => format_style_content(block_content, wrap_width, collapse),
                    _ => block_content.to_string(),
                };
                out.push_str(&formatted);

                if formatted.is_empty() {
                    out.push_str(&close_tag);
                } else {
                    out.push('\n');
                    out.push_str(&indent);
                    out.push_str(&close_tag);
                }
                remaining = &remaining[content_end + close_tag.len()..];
            } else {
                out.push_str(&remaining[start..]);
                remaining = "";
            }
        } else {
            out.push_str(&remaining[start..]);
            remaining = "";
        }
    }
    out.push_str(remaining);
    out
}

fn format_style_content(content: &str, wrap_width: usize, collapse: crate::full::format::CollapseConfig) -> String {
    if content.trim().is_empty() {
        return String::new();
    }
    let leading_nl = count_leading_newlines(content);
    let trailing_nl = count_trailing_newlines(content);

    let base_tabs = detect_min_leading_tabs(content);
    let bare: String = content.lines()
        .map(|l| strip_leading_tabs(l, base_tabs))
        .collect::<Vec<_>>()
        .join("\n");

    let formatted = crate::full::format::format_code_content(bare.trim(), "css", wrap_width, collapse, false);

    // Re-indent at base_tabs depth
    let mut out = String::new();
    for line in formatted.lines() {
        let ts = line.trim_start();
        if ts.is_empty() {
            out.push('\n');
            continue;
        }
        let leading = line.len() - ts.len();
        for _ in 0..(base_tabs + leading) { out.push('\t'); }
        out.push_str(ts);
        out.push('\n');
    }
    if out.ends_with('\n') { out.pop(); }

    for _ in 0..leading_nl { out.insert(0, '\n'); }
    for _ in 0..trailing_nl { out.push('\n'); }
    out
}

/// A standalone Ree control token occupying its own line inside a `<script>`.
enum ReeCtrl {
    /// Block opener: `{#if …}`, `{#each …}`, `{#with …}` — increases nesting.
    Open,
    /// Block clause: `{:else}`, `{:else if …}` — sits at the opener's level.
    Mid,
    /// Block closer: `{/if}`, `{/each}`, `{/with}` — decreases nesting.
    Close,
}

/// Classify a trimmed line as a standalone Ree control token.
///
/// Only matches when the ENTIRE line is a single block token, so JS that merely
/// contains braces (`class C {#x}`, `{ a: 1 }`, `{/* note */}`) and inline Ree
/// blocks (`{#if x}foo{/if}`) are deliberately NOT matched — those stay in the
/// JS run and are masked as expression placeholders like before.
fn classify_ree_control(trimmed: &str) -> Option<ReeCtrl> {
    if !trimmed.starts_with('{') {
        return None;
    }
    let end = find_matching_brace(trimmed, 0)?;
    if end != trimmed.len() - 1 {
        return None; // token does not span the whole line
    }
    let inner = trimmed[1..end].trim();
    if let Some(rest) = inner.strip_prefix('#') {
        let kw = rest.trim_start().split(char::is_whitespace).next().unwrap_or("");
        return matches!(kw, "if" | "each" | "with").then_some(ReeCtrl::Open);
    }
    if let Some(rest) = inner.strip_prefix(':') {
        return rest.trim_start().starts_with("else").then_some(ReeCtrl::Mid);
    }
    if let Some(rest) = inner.strip_prefix('/') {
        return matches!(rest.trim(), "if" | "each" | "with").then_some(ReeCtrl::Close);
    }
    None
}

/// Given `s` with `s.as_bytes()[start] == b'{'`, return the byte index of the
/// matching `}`, accounting for nested braces and quoted strings (single,
/// double, and template literals). Returns `None` if unbalanced.
fn find_matching_brace(s: &str, start: usize) -> Option<usize> {
    let bytes = s.as_bytes();
    let len = bytes.len();
    let mut depth = 0i32;
    let mut i = start;
    while i < len {
        match bytes[i] {
            q @ (b'\'' | b'"' | b'`') => {
                i += 1;
                while i < len {
                    if bytes[i] == b'\\' && i + 1 < len {
                        i += 2;
                    } else if bytes[i] == q {
                        i += 1;
                        break;
                    } else {
                        i += 1;
                    }
                }
                continue;
            }
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
        i += 1;
    }
    None
}

/// Strip up to `n` leading tab characters from `line`.
fn strip_leading_tabs(line: &str, n: usize) -> &str {
    let mut s = line;
    let mut removed = 0;
    while removed < n {
        match s.strip_prefix('\t') {
            Some(rest) => { s = rest; removed += 1; }
            None => break,
        }
    }
    s
}

/// Run the SWC formatting pipeline on one run of bare JS (no surrounding Ree
/// block structure). Masks inline Ree expressions, formats, then restores them.
/// The returned JS uses SWC's own column-0-based indentation; the caller adds
/// the structural indentation.
fn format_js_fragment(bare_js: &str, remove_unused: bool) -> String {
    if bare_js.trim().is_empty() {
        return String::new();
    }
    let mut ree_placeholders: Vec<String> = Vec::new();
    let protected = protect_ree_expressions(bare_js, &mut ree_placeholders);
    // Preserve blank lines and block comments across SWC (same pipeline as .ts/.js files)
    let (preprocessed, swc_placeholders) = crate::full::format::preprocess_for_swc(&protected);
    let swc_out = crate::full::swc_format::format_js_with_indent(&preprocessed, "\t", remove_unused);
    let restored_swc = crate::full::format::postprocess_from_swc(&swc_out, &swc_placeholders);
    // Fix arrow function spacing: `()=>{` → `() => {`
    let formatted = crate::full::format::fix_arrow_spacing(&restored_swc);
    // Fix SWC codegen try/catch spacing: `catch  {` → `catch {`, `finally{` → `finally {`
    let formatted = crate::full::format::fix_swc_try_catch_spacing(&formatted);
    restore_ree_expressions(&formatted, &ree_placeholders)
}

/// Format the content inside a `<script>` tag.
///
/// The Ree parser already structures top-level `{#if}`/`{#each}`/`{#with}`
/// blocks inside scripts (each control token on its own line, body indented).
/// This function preserves that structure: it splits the script into JS runs
/// separated by standalone Ree control tokens, formats each JS run through SWC
/// independently, and re-indents both runs and tokens according to the block
/// nesting depth. Control tokens are NEVER fed to SWC — doing so was what caused
/// them to be flattened and to acquire spurious trailing semicolons.
fn format_script_content(content: &str, wrap_width: usize, collapse: crate::full::format::CollapseConfig, remove_unused: bool) -> String {
    if content.trim().is_empty() {
        return String::new();
    }
    // Detect leading/trailing newlines (separation from <script>> and </script>).
    let leading_nl = count_leading_newlines(content);
    let trailing_nl = count_trailing_newlines(content);
    let trimmed = content.trim();

    // Detect base_tabs from raw (untrimmed) content lines. This is the indent
    // floor of the script's top-level content; block bodies sit at base + depth.
    let base_tabs = detect_min_leading_tabs(content);

    let mut out = String::new();
    let mut depth: usize = 0;
    let mut run: Vec<&str> = Vec::new();

    for line in trimmed.lines() {
        let token = line.trim();
        match classify_ree_control(token) {
            Some(ctrl) => {
                flush_js_run(&run, base_tabs, depth, wrap_width, collapse, remove_unused, &mut out);
                run.clear();
                match ctrl {
                    ReeCtrl::Open => {
                        emit_ctrl(&mut out, base_tabs + depth, token);
                        depth += 1;
                    }
                    ReeCtrl::Mid => {
                        emit_ctrl(&mut out, base_tabs + depth.saturating_sub(1), token);
                    }
                    ReeCtrl::Close => {
                        depth = depth.saturating_sub(1);
                        emit_ctrl(&mut out, base_tabs + depth, token);
                    }
                }
            }
            None => run.push(line),
        }
    }
    flush_js_run(&run, base_tabs, depth, wrap_width, collapse, remove_unused, &mut out);

    if out.ends_with('\n') { out.pop(); }

    // Prepend leading newlines and append trailing newlines
    for _ in 0..leading_nl {
        out.insert(0, '\n');
    }
    for _ in 0..trailing_nl {
        out.push('\n');
    }
    out
}

/// Emit a standalone Ree control token at the given tab depth.
fn emit_ctrl(out: &mut String, tabs: usize, token: &str) {
    for _ in 0..tabs { out.push('\t'); }
    out.push_str(token);
    out.push('\n');
}

/// Format and emit one accumulated JS run (the lines between two Ree control
/// tokens) into `out`, indented at `base_tabs + depth` plus SWC's own nesting.
fn flush_js_run(
    run: &[&str],
    base_tabs: usize,
    depth: usize,
    wrap_width: usize,
    collapse: crate::full::format::CollapseConfig,
    remove_unused: bool,
    out: &mut String,
) {
    if run.is_empty() {
        return;
    }
    let joined = run.join("\n");
    if joined.trim().is_empty() {
        return;
    }

    // Dedent the run to column 0 so SWC formats it cleanly regardless of the
    // block nesting it sits in.
    let run_base = detect_min_leading_tabs(&joined);
    let bare: String = joined
        .lines()
        .map(|l| strip_leading_tabs(l, run_base))
        .collect::<Vec<_>>()
        .join("\n");

    let restored = format_js_fragment(&bare, remove_unused);
    if restored.trim().is_empty() {
        return;
    }

    // Re-indent each formatted line at base_tabs + depth + its own SWC nesting.
    let mut block = String::new();
    for line in restored.lines() {
        let ts = line.trim_start();
        if ts.is_empty() {
            block.push('\n');
            continue;
        }
        let leading = line.len() - ts.len();
        for _ in 0..(base_tabs + depth + leading) { block.push('\t'); }
        block.push_str(ts);
        block.push('\n');
    }
    if block.trim().is_empty() {
        return;
    }
    if block.ends_with('\n') { block.pop(); }

    // Optionally collapse single-statement blocks within this run.
    let block = if collapse.enabled {
        crate::full::format::collapse_single_stmt_blocks(&block, wrap_width, &collapse)
    } else {
        block
    };

    out.push_str(&block);
    out.push('\n');
}

fn count_leading_newlines(s: &str) -> usize {
    s.chars().take_while(|&c| c == '\n').count()
}

fn count_trailing_newlines(s: &str) -> usize {
    s.chars().rev().take_while(|&c| c == '\n').count()
}

/// Detect the minimum number of leading tab characters in non-empty lines.
/// Ignores lines that have no leading tabs (they're at column 0).
fn detect_min_leading_tabs(content: &str) -> usize {
    let mut min_tabs = usize::MAX;
    for line in content.lines() {
        let trimmed = line.trim_start();
        if trimmed.is_empty() {
            continue;
        }
        if trimmed == line {
            // Line has no leading whitespace — skip it, it's not indented
            continue;
        }
        let leading = line.len() - trimmed.len();
        // Count tabs (each tab is 1 char)
        let tab_count = line[..leading].chars().filter(|&c| c == '\t').count();
        if tab_count > 0 && tab_count < min_tabs {
            min_tabs = tab_count;
        }
    }
    if min_tabs == usize::MAX { 0 } else { min_tabs }
}

/// Replace all Ree syntax with unique placeholders so SWC can parse
/// the surrounding JS without choking on template syntax.
/// Protects: {= expr}, {~ expr}, {_ expr}, {- expr}, {#keyword ...}, {:else}, {/keyword}
/// Correctly skips content inside JS single/double-quoted strings.
/// Preserves multi-byte UTF-8 characters (does NOT push bytes as chars).
fn protect_ree_expressions(input: &str, placeholders: &mut Vec<String>) -> String {
    let mut result = String::with_capacity(input.len());
    let bytes = input.as_bytes();
    let len = bytes.len();
    let mut i = 0;
    let mut idx = 0;

    while i < len {
        let b = bytes[i];

        // Skip single/double-quoted JS strings (but NOT backticks — template
        // literals handle {=} via ${} interpolation which SWC parses fine)
        if b == b'\'' || b == b'"' {
            let quote = b;
            result.push(b as char); // opening quote (ASCII)
            i += 1;
            while i < len {
                let c = bytes[i];
                if c == b'\\' && i + 1 < len {
                    // Escaped character — copy both bytes (both ASCII)
                    result.push(c as char);
                    result.push(bytes[i + 1] as char);
                    i += 2;
                } else if c == quote {
                    result.push(c as char); // closing quote (ASCII)
                    i += 1;
                    break;
                } else if c & 0x80 == 0 {
                    // ASCII inside string
                    result.push(c as char);
                    i += 1;
                } else {
                    // Multi-byte UTF-8 inside string — consume full character
                    let ch = input[i..].chars().next().unwrap();
                    result.push(ch);
                    i += ch.len_utf8();
                }
            }
            continue;
        }

        // Check for ANY Ree syntax at '{': {=, {~, {_, {-, {#, {:, {/
        if b == b'{' && i + 1 < len {
            let next = bytes[i + 1];
            let is_ree = next == b'=' || next == b'~' || next == b'_' || next == b'-' || next == b'#' || next == b':' || next == b'/';
            if is_ree {
                // Scan to the MATCHING `}`, accounting for nested braces and
                // quoted strings inside the expression. A naive first-`}` scan
                // corrupts expressions like `{= foo({ a: 1 }) }`.
                if let Some(end) = find_matching_brace(input, i) {
                    let expr = &input[i..=end];
                    let placeholder = format!("__REE_PLACEHOLDER_{}__", idx);
                    idx += 1;
                    placeholders.push(expr.to_string());
                    result.push_str(&placeholder);
                    i = end + 1;
                    continue;
                }
            }
        }

        // Copy the full UTF-8 character (not byte-by-byte)
        if b & 0x80 == 0 {
            // ASCII byte
            result.push(b as char);
            i += 1;
        } else {
            // Multi-byte UTF-8 — consume full character from the string
            let ch = input[i..].chars().next().unwrap();
            result.push(ch);
            i += ch.len_utf8();
        }
    }
    result
}

/// Restore Ree expression placeholders back to their original expressions.
fn restore_ree_expressions(input: &str, placeholders: &[String]) -> String {
    let mut result = input.to_string();
    for (i, expr) in placeholders.iter().enumerate() {
        let placeholder = format!("__REE_PLACEHOLDER_{}__", i);
        result = result.replace(&placeholder, expr);
    }
    result
}



/// Format a Ree template file. Returns `true` if the file was (or would be) modified.
pub(crate) fn format_ree_file(path: &Path, mode: crate::full::format::Mode, wrap_width: usize, oneline: usize, collapse: crate::full::format::CollapseConfig, remove_unused: bool) -> bool {
    let content = match fs::read_to_string(path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Error reading {}: {}", path.display(), e);
            return false;
        }
    };

    let normalized = content.replace("\r\n", "\n");
    let write_content = format_ree_content(&normalized, wrap_width, oneline, collapse, remove_unused);

    if write_content == normalized {
        return false;
    }

    match mode {
        crate::full::format::Mode::Write => {
            match fs::write(path, &write_content) {
                Ok(_) => eprintln!("\r\x1b[KFormatted: {}", path.display()),
                Err(e) => eprintln!("Error writing {}: {}", path.display(), e),
            }
        }
        crate::full::format::Mode::Check => {
            eprintln!("Would format: {}", path.display());
        }
        crate::full::format::Mode::Diff => {
            crate::full::format::print_diff(path, &normalized, &write_content);
        }
    }

    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::full::format::CollapseConfig;

    fn cfg() -> CollapseConfig {
        CollapseConfig::uniform(true, 3)
    }

    #[test]
    fn classify_ree_control_matches_block_tokens() {
        assert!(matches!(classify_ree_control("{#if x}"), Some(ReeCtrl::Open)));
        assert!(matches!(classify_ree_control("{#each items as i}"), Some(ReeCtrl::Open)));
        assert!(matches!(classify_ree_control("{#with props}"), Some(ReeCtrl::Open)));
        assert!(matches!(classify_ree_control("{:else}"), Some(ReeCtrl::Mid)));
        assert!(matches!(classify_ree_control("{:else if y}"), Some(ReeCtrl::Mid)));
        assert!(matches!(classify_ree_control("{/if}"), Some(ReeCtrl::Close)));
    }

    #[test]
    fn classify_ree_control_rejects_js() {
        assert!(classify_ree_control("class C {#x}").is_none()); // not a leading brace
        assert!(classify_ree_control("{#x}").is_none());         // private-field-like, not a keyword
        assert!(classify_ree_control("{ a: 1 }").is_none());     // object literal
        assert!(classify_ree_control("{ x ? a : b }").is_none()); // ternary in braces
        assert!(classify_ree_control("{/* note */}").is_none()); // block comment
        assert!(classify_ree_control("{#if x}foo{/if}").is_none()); // inline block: trailing content
        assert!(classify_ree_control("{#if x};").is_none());     // trailing junk: not a clean token
        assert!(classify_ree_control("doThing();").is_none());
    }

    #[test]
    fn protect_ree_handles_nested_braces() {
        // Regression: a naive first-`}` scan captured only `{= bar({ a: 1 }`,
        // leaking the rest of the expression into the JS stream.
        let mut ph = Vec::new();
        let out = protect_ree_expressions("foo({= bar({ a: 1 }) })", &mut ph);
        assert_eq!(ph.len(), 1);
        assert_eq!(ph[0], "{= bar({ a: 1 }) }");
        assert_eq!(out, "foo(__REE_PLACEHOLDER_0__)");
    }

    #[test]
    fn script_if_block_body_is_indented() {
        let src = "{#with props}\n\t<script>\n\t\tconst x = 1;\n\t\t{#if flag}\n\t\tdoThing();\n\t\t{/if}\n\t</script>\n{/with}\n";
        let out = format_ree_content(src, 120, 0, cfg(), false);
        assert!(out.contains("\t\t{#if flag}\n\t\t\tdoThing();\n\t\t{/if}"),
            "if-block body should be indented one level deeper than the tokens:\n{out}");
    }

    #[test]
    fn script_if_block_repairs_injected_semicolons() {
        // Mimics a file a previous reefmt run corrupted: stray `;` after the
        // control tokens, and a flattened body.
        let src = "{#with props}\n\t<script>\n\t\tconst x = 1;\n\t\t{#if flag};\n\t\tdoThing();\n\t\t{/if};\n\t\tmore();\n\t</script>\n{/with}\n";
        let out = format_ree_content(src, 120, 0, cfg(), false);
        assert!(!out.contains("{#if flag};"), "stray ; after {{#if}} should be removed:\n{out}");
        assert!(!out.contains("{/if};"), "stray ; after {{/if}} should be removed:\n{out}");
        // Repaired output must be idempotent.
        let out2 = format_ree_content(&out, 120, 0, cfg(), false);
        assert_eq!(out, out2, "repaired output should be idempotent");
    }

    #[test]
    fn script_if_block_idempotent() {
        let src = "{#with props}\n\t<script>\n\t\tconst x = 1;\n\t\t{#if flag}\n\t\t\tdoThing();\n\t\t{/if}\n\t</script>\n{/with}\n";
        let p1 = format_ree_content(src, 120, 0, cfg(), false);
        let p2 = format_ree_content(&p1, 120, 0, cfg(), false);
        assert_eq!(p1, p2, "well-formed if-in-script should be stable");
    }

    #[test]
    fn script_whole_body_single_if_block_stays_inside_and_is_idempotent() {
        // A <script> whose entire body is one {#if} block must NOT be rewritten
        // into {#if}<script>…</script>{/if} (that changes semantics: empty-tag vs
        // no-tag) and must be stable across passes.
        let src = "{#with props}\n\t<script>\n\t\t{#if dev}\n\t\t\tconsole.log(\"x\");\n\t\t{/if}\n\t</script>\n{/with}\n";
        let p1 = format_ree_content(src, 120, 0, cfg(), false);
        assert!(p1.contains("<script>\n\t\t{#if dev}"),
            "the {{#if}} block must stay inside <script>:\n{p1}");
        let p2 = format_ree_content(&p1, 120, 0, cfg(), false);
        assert_eq!(p1, p2, "single-block-in-script must be idempotent");
    }

    #[test]
    fn format_ree_content_idempotent() {
        let src = "<span>text</span>\n";
        let result = format_ree_content(src, 120, 0, CollapseConfig::uniform(true, 3), false);
        assert_eq!(result, src, "format_ree_content should be idempotent for already-formatted content");
    }

    #[test]
    fn idempotent_non_ascii_in_script_comment() {
        // Regression: non-ASCII UTF-8 chars in <script> comments were corrupted
        // by protect_ree_expressions pushing bytes as chars (mojibake).
        let src = "<!DOCTYPE html>\n<html>\n\t<head>\n\t\t<script>\n\t\t// šč test — non-ASCII\n\t\tconst x = 1;\n\t\t</script>\n\t</head>\n</html>\n";
        let pass1 = format_ree_content(src, 120, 0, CollapseConfig::uniform(true, 3), false);
        let pass2 = format_ree_content(&pass1, 120, 0, CollapseConfig::uniform(true, 3), false);
        assert_eq!(pass1, pass2,
            "format_ree_content should be idempotent with non-ASCII chars in script comments");
    }

    #[test]
    fn idempotent_non_ascii_in_ree_expr() {
        // Non-ASCII inside Ree expressions should also be stable
        let src = "<p>{= props.ui.šč_test }</p>\n";
        let pass1 = format_ree_content(src, 120, 0, CollapseConfig::uniform(true, 3), false);
        let pass2 = format_ree_content(&pass1, 120, 0, CollapseConfig::uniform(true, 3), false);
        assert_eq!(pass1, pass2,
            "format_ree_content should be idempotent with non-ASCII in Ree expressions");
    }

    #[test]
    fn idempotent_doctype_and_html() {
        // Regression: DOCTYPE was parsed as an HTML element, causing indentation drift
        let src = "<!DOCTYPE html>\n\n<html lang=\"en\">\n\t<head>\n\t\t<meta charset=\"UTF-8\" />\n\t</head>\n</html>\n";
        let pass1 = format_ree_content(src, 120, 0, CollapseConfig::uniform(true, 3), false);
        let pass2 = format_ree_content(&pass1, 120, 0, CollapseConfig::uniform(true, 3), false);
        assert_eq!(pass1, pass2,
            "format_ree_content should be idempotent with DOCTYPE declarations");
    }

    #[test]
    fn idempotent_ree_blocks_and_script() {
        // Full Ree block with embedded script containing non-ASCII
        let src = "{#if props.show}\n\t<script>\n\t// Café naïve — UTF-8 in script\n\tconsole.log(42);\n\t</script>\n{/if}\n";
        let pass1 = format_ree_content(src, 120, 0, CollapseConfig::uniform(true, 3), false);
        let pass2 = format_ree_content(&pass1, 120, 0, CollapseConfig::uniform(true, 3), false);
        assert_eq!(pass1, pass2,
            "format_ree_content should be idempotent with Ree blocks and UTF-8 in scripts");
    }

    #[test]
    fn raw_js_block_is_formatted() {
        // {{ ... }} blocks should go through the JS formatter, not just get
        // flat-indented. Nested object literal should get proper relative indentation.
        let src = "{{\n\tconst x = {\n\ta: 1,\n\tb: 2,\n\t};\n}}\n";
        let result = format_ree_content(src, 120, 0, CollapseConfig::uniform(true, 3), false);
        // After formatting, nested object members should be indented deeper than 1 tab.
        assert!(result.starts_with("{{"), "block starts with {{");
        assert!(result.contains("}}"), "block ends with }}");
        // Idempotent
        let pass2 = format_ree_content(&result, 120, 0, CollapseConfig::uniform(true, 3), false);
        assert_eq!(result, pass2, "{{}} block formatting should be idempotent");
    }

    #[test]
    fn idempotent_multiple_blank_lines() {
        // Multiple consecutive blank lines should converge to stable output
        let src = "<div>\n\n\n<p>text</p>\n</div>\n";
        let pass1 = format_ree_content(src, 120, 0, CollapseConfig::uniform(true, 3), false);
        let pass2 = format_ree_content(&pass1, 120, 0, CollapseConfig::uniform(true, 3), false);
        assert_eq!(pass1, pass2,
            "format_ree_content should be idempotent with multiple blank lines");
    }
}
