//! File discovery — which files to format. Lifted from reefmt's proven walker
//! (skipDirs / skipDotDirs / skipFiles / skipExtensions / extensions), trimmed of
//! anything formatting-related.

use crate::config::Config;
use std::path::{Path, PathBuf};

/// True if `path` sits inside a skipped directory (by name or dot-prefix).
pub fn is_in_skipped_dir(path: &Path, config: &Config) -> bool {
    path.components().any(|c| {
        if let std::path::Component::Normal(s) = c {
            if let Some(name) = s.to_str() {
                if config.skip_dirs.iter().any(|d| d == name) {
                    return true;
                }
                if config.skip_dot_dirs && name.starts_with('.') && name != "." {
                    return true;
                }
            }
        }
        false
    })
}

/// True if the file name ends with a configured compound skip-extension.
pub fn has_skipped_extension(path: &Path, config: &Config) -> bool {
    let name = match path.file_name().and_then(|s| s.to_str()) {
        Some(n) => n,
        None => return false,
    };
    config
        .skip_extensions
        .iter()
        .any(|ext| name.ends_with(&format!(".{}", ext)))
}

/// True if the file path matches any `skipFiles` glob (relative to cwd).
pub fn matches_skip_glob(path: &Path, config: &Config) -> bool {
    if config.skip_files.is_empty() {
        return false;
    }
    let rel = std::env::current_dir()
        .ok()
        .and_then(|cwd| path.strip_prefix(&cwd).ok().map(|p| p.to_path_buf()))
        .unwrap_or_else(|| path.to_path_buf());
    let s = rel.to_string_lossy().replace('\\', "/");
    config
        .skip_files
        .iter()
        .any(|pat| glob::Pattern::new(pat).map(|p| p.matches(&s)).unwrap_or(false))
}

/// True if a file with this extension is one we format and isn't skipped.
pub fn is_formattable(path: &Path, config: &Config) -> bool {
    match path.extension().and_then(|s| s.to_str()) {
        Some(ext) => {
            config.extensions.iter().any(|e| e == ext)
                && !has_skipped_extension(path, config)
                && !matches_skip_glob(path, config)
        }
        None => false,
    }
}

/// Recursively collect formattable files under `dir`.
pub fn collect(dir: &Path, out: &mut Vec<PathBuf>, config: &Config) -> std::io::Result<()> {
    if !dir.is_dir() {
        return Ok(());
    }
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            if is_in_skipped_dir(&path, config) {
                continue;
            }
            collect(&path, out, config)?;
        } else if is_formattable(&path, config) {
            out.push(path);
        }
    }
    Ok(())
}
