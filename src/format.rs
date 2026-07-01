//! Format dispatch by extension.
//!
//! The engine is being built rule-by-rule. Until each language path is wired to
//! the token engine, it passes content through unchanged (never corrupts —
//! graceful degradation is the whole point; see CONTEXT.md).

use crate::config::Config;

pub fn format_source(content: &str, ext: &str, config: &Config) -> String {
    match ext {
        "ts" | "js" => format_js(content, config),
        "css" => format_css(content, config),
        "ree" => format_ree(content, config),
        // Unknown extension: never touch it.
        _ => content.to_string(),
    }
}

fn format_js(content: &str, config: &Config) -> String {
    crate::engine::format_js(content, &config.indent)
}

fn format_css(content: &str, config: &Config) -> String {
    crate::engine::format_css(content, &config.indent)
}

fn format_ree(content: &str, config: &Config) -> String {
    crate::ree::format_ree(content, &config.indent)
}
