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
    /// two spaces. This is the only formatting knob — the whole point of the
    /// rewrite is that the author steers line breaks, not config.
    pub indent: String,
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
