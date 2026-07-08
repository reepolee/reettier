//! The `--full` Reprinter: reefmt's AST-reprinting engine, vendored into
//! reettier. It discards the author's line breaks and re-derives layout from
//! the syntax tree (SWC for JS/TS, malva for CSS). See docs/adr/0001.
//!
//! reettier reaches this engine only through `format_full` below; per-file IO
//! and discovery stay in `main.rs`. The `removeUnusedImports` behavior from
//! reefmt is never enabled here (no formatting mode edits semantics) - the
//! `remove_unused` parameter threaded through the vendored code is always
//! `false`. See docs/adr/0001 and CONTEXT.md "reettier.jsonc".

// The vendored reprint engine carries a few file-IO helpers (format_code_file,
// format_ree_file, print_diff, Mode) that reettier's main.rs supersedes - they
// stay for now (minimal change) but are unreachable, so allow dead_code here.
#![allow(dead_code)]

pub(crate) mod ast_check;
pub(crate) mod format;
pub(crate) mod ree_format;
pub(crate) mod ree_parser;
pub(crate) mod remove_unused_imports;
pub(crate) mod swc_format;
pub(crate) mod swc_printer;

use crate::config::FullConfig;

/// Reprint one source string by extension. Mirrors reefmt's `--stdin` dispatch:
/// `.ree` goes through the template pipeline, `.ts/.js/.css` through the code
/// pipeline. Unknown extensions are returned unchanged.
///
/// Self-verify (docs/adr/0002): after reprinting, the output is re-checked
/// against the input for semantic-token and comment preservation. If the check
/// fails, the reprint is discarded and the **original** `content` is returned
/// unchanged, so a corrupting format is never propagated to the caller.
pub fn format_full(content: &str, ext: &str, full: &FullConfig) -> String {
    // remove_unused is always false - the config surface for it was dropped.
    let remove_unused = false;
    let collapse = full.collapse_config();
    let formatted = match ext {
        "ree" => ree_format::format_ree_content(
            content,
            full.wrap_width,
            full.oneline,
            collapse,
            remove_unused,
        ),
        "ts" | "js" | "css" => {
            format::format_code_content(content, ext, full.wrap_width, collapse, remove_unused)
        }
        _ => return content.to_string(),
    };

    if let Err(msg) = verify_reprint(content, &formatted, ext) {
        eprintln!(
            "\x1b[1;31mreettier --full would corrupt this file:\x1b[0m {}",
            msg
        );
        eprintln!("Left unchanged. Please report this as a bug.");
        return content.to_string();
    }

    formatted
}

/// Reprinter self-verify: semantic AST tokens (TS/JS only) plus comments (all
/// types) must be preserved. Whitespace and line breaks are ignored, so the
/// reprint's re-layout passes - only real token loss or a dropped comment fails.
fn verify_reprint(original: &str, formatted: &str, ext: &str) -> Result<(), String> {
    if matches!(ext, "ts" | "js") {
        ast_check::verify_semantics_preserved(original, formatted)?;
    }
    ast_check::verify_comments_preserved(original, formatted, ext)
}
