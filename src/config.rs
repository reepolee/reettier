//! reettier configuration.
//!
//! The layout-preserving engine has no width/collapse knobs (see ADR-0001 in the
//! reefmt repo — this is the clean-room rewrite). All that remains is file
//! discovery plus a single formatting preference: the indent string.
//!
//! The config file (`reettier.jsonc`) is **optional** — with no file present,
//! sane defaults apply.

use serde::Deserialize;
use std::path::Path;

fn default_skip_dirs() -> Vec<String> {
    ["node_modules", "vendor", "vendors", "dist", "static", "templates"]
        .iter()
        .map(|s| s.to_string())
        .collect()
}
fn default_skip_extensions() -> Vec<String> {
    vec!["min.js".to_string(), "min.css".to_string()]
}
fn default_extensions() -> Vec<String> {
    ["ree", "ts", "js", "css"].iter().map(|s| s.to_string()).collect()
}
fn default_true() -> bool {
    true
}
/// Default indent is a hard tab (see CONTEXT.md "Markup indentation").
fn default_indent() -> String {
    "\t".to_string()
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Directory names to skip anywhere in the tree.
    #[serde(rename = "skipDirs")]
    pub skip_dirs: Vec<String>,
    /// Glob patterns (relative to cwd) for files to skip.
    #[serde(rename = "skipFiles")]
    pub skip_files: Vec<String>,
    /// Compound extensions always skipped (e.g. `min.js`).
    #[serde(rename = "skipExtensions")]
    pub skip_extensions: Vec<String>,
    /// File extensions to format.
    pub extensions: Vec<String>,
    /// Skip directories whose name starts with a dot.
    #[serde(rename = "skipDotDirs")]
    pub skip_dot_dirs: bool,
    /// The indent string. `"\t"` (default) for a hard tab, or e.g. `"  "` for
    /// two spaces. This is the only Indenter formatting knob — the whole point
    /// of the rewrite is that the author steers line breaks, not config.
    pub indent: String,
    /// Reprinter (`--full`) knobs. These only apply when formatting with
    /// `--full`; the default Indenter never reads them. See docs/adr/0001.
    pub full: FullConfig,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            skip_dirs: default_skip_dirs(),
            skip_files: Vec::new(),
            skip_extensions: default_skip_extensions(),
            extensions: default_extensions(),
            skip_dot_dirs: default_true(),
            indent: default_indent(),
            full: FullConfig::default(),
        }
    }
}

fn default_wrap_width() -> usize {
    100
}
fn default_collapse_max_members() -> usize {
    4
}
fn default_soft_width() -> usize {
    100
}
fn default_tab_width() -> usize {
    4
}
fn default_keyvalue_props() -> usize {
    1
}

/// Reprinter-only configuration (the `"full"` block of `reettier.jsonc`).
/// Ported from reefmt's flat config; `removeUnusedImports` is intentionally
/// absent (no formatting mode edits semantics - see docs/adr/0001).
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct FullConfig {
    /// Maximum line width before elements are broken onto multiple lines.
    #[serde(rename = "wrapWidth")]
    pub wrap_width: usize,
    /// When true, single-statement blocks and object-literal function params are
    /// collapsed onto one line when they fit within wrapWidth.
    #[serde(rename = "collapseSingleStatementBlocks")]
    pub collapse_single_stmt_blocks: bool,
    /// Global fallback member limit; any per-category limit left unset uses it.
    #[serde(rename = "collapseMaxMembers")]
    pub collapse_max_members: usize,
    #[serde(rename = "collapseMaxObjectMembers")]
    pub collapse_max_object_members: Option<usize>,
    #[serde(rename = "collapseMaxArrayElements")]
    pub collapse_max_array_elements: Option<usize>,
    #[serde(rename = "collapseMaxFunctionParams")]
    pub collapse_max_function_params: Option<usize>,
    #[serde(rename = "collapseMaxCallArgs")]
    pub collapse_max_call_args: Option<usize>,
    #[serde(rename = "collapseMaxImports")]
    pub collapse_max_imports: Option<usize>,
    #[serde(rename = "collapseMaxTypeMembers")]
    pub collapse_max_type_members: Option<usize>,
    /// Soft wrap width: any collapsible structure whose inline form fits within
    /// this width stays on one line regardless of the count caps. 0 disables.
    #[serde(rename = "collapseSoftWidth")]
    pub collapse_soft_width: usize,
    /// Display width of a tab, used to measure line widths for wrap/collapse.
    #[serde(rename = "tabWidth")]
    pub tab_width: usize,
    /// Max `key: value` props an object literal may have and still collapse.
    #[serde(rename = "collapseMaxKeyValueProps")]
    pub collapse_max_keyvalue_props: usize,
    /// Argument-hugging: `({ ... })` on the callee line for a single object/array
    /// argument that does not fit inline.
    #[serde(rename = "hugCallArgs")]
    pub hug_call_args: bool,
    /// Width threshold for collapsing multi-line HTML leaf elements onto one
    /// line in `.ree` files. 0 disables.
    pub oneline: usize,
}

impl Default for FullConfig {
    fn default() -> Self {
        FullConfig {
            wrap_width: default_wrap_width(),
            collapse_single_stmt_blocks: default_true(),
            collapse_max_members: default_collapse_max_members(),
            collapse_max_object_members: None,
            collapse_max_array_elements: None,
            collapse_max_function_params: None,
            collapse_max_call_args: None,
            collapse_max_imports: None,
            collapse_max_type_members: None,
            collapse_soft_width: default_soft_width(),
            tab_width: default_tab_width(),
            collapse_max_keyvalue_props: default_keyvalue_props(),
            hug_call_args: false,
            oneline: 0,
        }
    }
}

impl FullConfig {
    /// Build the Reprinter's `CollapseConfig`, applying per-category fallback to
    /// `collapse_max_members`.
    pub(crate) fn collapse_config(&self) -> crate::full::format::CollapseConfig {
        let def = self.collapse_max_members;
        crate::full::format::CollapseConfig {
            enabled: self.collapse_single_stmt_blocks,
            max_object_members: self.collapse_max_object_members.unwrap_or(def),
            max_array_elements: self.collapse_max_array_elements.unwrap_or(def),
            max_function_params: self.collapse_max_function_params.unwrap_or(def),
            max_call_args: self.collapse_max_call_args.unwrap_or(def),
            max_imports: self.collapse_max_imports.unwrap_or(def),
            max_type_members: self.collapse_max_type_members.unwrap_or(def),
            soft_wrap_width: self.collapse_soft_width,
            tab_width: self.tab_width,
            max_keyvalue_props: self.collapse_max_keyvalue_props,
            collapse_width: self.oneline,
            hug_call_args: self.hug_call_args,
        }
    }
}

impl Config {
    /// Load `reettier.jsonc` from `dir` if it exists; otherwise return defaults.
    /// Unlike reefmt, a missing config is **not** an error.
    pub fn load(dir: &Path) -> Result<Config, String> {
        let path = dir.join("reettier.jsonc");
        if !path.exists() {
            return Ok(Config::default());
        }
        let content = std::fs::read_to_string(&path)
            .map_err(|e| format!("could not read {}: {}", path.display(), e))?;
        json5::from_str(&content)
            .map_err(|e| format!("invalid {}: {}", path.display(), e))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_sane() {
        let c = Config::default();
        assert_eq!(c.indent, "\t");
        assert!(c.extensions.contains(&"ree".to_string()));
        assert!(c.skip_dirs.contains(&"node_modules".to_string()));
        assert!(c.skip_dot_dirs);
    }

    #[test]
    fn partial_config_fills_defaults() {
        // Only `indent` set — everything else should default.
        let c: Config = json5::from_str(r#"{ "indent": "  " }"#).unwrap();
        assert_eq!(c.indent, "  ");
        assert!(c.extensions.contains(&"css".to_string()));
    }
}
