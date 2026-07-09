//! reettier — a formatter for `.ree` templates and their embedded JS/TS/CSS.
//! Default mode is the layout-preserving Indenter (the author steers line
//! breaks). `--full` selects the Reprinter, which re-derives layout from the
//! syntax tree (the former `reefmt` engine, now vendored under `full/`).
//! See CONTEXT.md and docs/adr/0001, 0002 for the design.

mod config;
mod discovery;
mod engine;
mod format;
mod full;
mod ree;
mod tokenizer;

use config::Config;
use rayon::prelude::*;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Write,
    Check,
    Diff,
}

fn executable_dir() -> PathBuf {
    let exe = std::env::current_exe().unwrap_or_else(|e| {
        eprintln!("Error: cannot determine executable path: {}", e);
        std::process::exit(1);
    });

    exe.parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| {
            eprintln!("Error: executable path has no parent directory");
            std::process::exit(1);
        })
}

fn main() {
    let mut args: Vec<String> = std::env::args().skip(1).collect();

    let take = |args: &mut Vec<String>, names: &[&str]| -> bool {
        let hit = args.iter().any(|a| names.contains(&a.as_str()));
        args.retain(|a| !names.contains(&a.as_str()));
        hit
    };

    if take(&mut args, &["--help", "-h"]) {
        print_help();
        return;
    }
    if take(&mut args, &["--version", "-v"]) {
        println!("{}", env!("CARGO_PKG_VERSION"));
        return;
    }
    if take(&mut args, &["--where"]) {
        println!("{}", executable_dir().display());
        return;
    }

    // Hidden dev mode: print running bracket depth per source line, to locate
    // where balance goes wrong (a mis-scanned regex/template/comment).
    if take(&mut args, &["--depths"]) {
        let mut input = String::new();
        let _ = std::io::stdin().read_to_string(&mut input);
        let mut depth: i64 = 0;
        let mut line = 1;
        for t in tokenizer::tokenize(&input) {
            use tokenizer::TokKind::*;
            match t.kind {
                Open => depth += 1,
                Close => depth -= 1,
                Newline => {
                    println!("{:>4}: depth={}", line, depth);
                    line += 1;
                }
                _ => {}
            }
        }
        return;
    }

    // Hidden dev mode: dump the significant token stream from stdin (one per
    // line), skipping whitespace and commas. Used to prove that formatting
    // preserves every meaningful token (no loss/reorder — commas are managed).
    if take(&mut args, &["--dump-sig"]) {
        let mut input = String::new();
        let _ = std::io::stdin().read_to_string(&mut input);
        for t in tokenizer::tokenize(&input) {
            use tokenizer::TokKind::*;
            if matches!(t.kind, Space | Newline | Comma) {
                continue;
            }
            println!("{:?}\t{}", t.kind, t.text(&input));
        }
        return;
    }

    if take(&mut args, &["--init"]) {
        run_init();
        return;
    }

    let full_mode = take(&mut args, &["--full"]);
    let diff_mode = take(&mut args, &["--diff"]);
    let check_mode = take(&mut args, &["--check", "--dry-run", "-c"]);
    let verbose = take(&mut args, &["--verbose"]);
    let git_mode = take(&mut args, &["--git"]);

    // --stdin [.ext] — read stdin, write stdout.
    let stdin_mode = args.iter().position(|a| a == "--stdin");
    let stdin_ext: Option<String> = stdin_mode.map(|pos| {
        args.remove(pos);
        match args.first() {
            Some(first) if first.starts_with('.') => args.remove(0),
            Some(first) if Path::new(first).extension().is_some() => {
                let ext = format!(".{}", Path::new(first).extension().unwrap().to_string_lossy());
                args.remove(0);
                ext
            }
            _ => ".ree".to_string(),
        }
    });

    let cwd = std::env::current_dir().unwrap_or_else(|e| {
        eprintln!("Error: cannot determine current directory: {}", e);
        std::process::exit(1);
    });

    let config = Config::load(&cwd).unwrap_or_else(|e| {
        eprintln!("Error: {}", e);
        std::process::exit(1);
    });

    if let Some(ext) = stdin_ext {
        run_stdin(&ext, &config, full_mode);
        return;
    }

    let mode = if diff_mode {
        Mode::Diff
    } else if check_mode {
        Mode::Check
    } else {
        Mode::Write
    };

    let targets: Vec<String> = if args.is_empty() { vec![".".into()] } else { args };

    let files = if git_mode {
        git_changed_files(&config)
    } else {
        collect_targets(&targets, &config)
    };

    if files.is_empty() {
        return;
    }

    let changed = AtomicU64::new(0);
    let start = std::time::Instant::now();

    files.par_iter().for_each(|file| {
        match format_one(file, mode, &config, full_mode) {
            Ok(true) => {
                changed.fetch_add(1, Ordering::Relaxed);
            }
            Ok(false) => {
                if verbose {
                    eprintln!("Already formatted: {}", file.display());
                }
            }
            Err(e) => eprintln!("Error formatting {}: {}", file.display(), e),
        }
    });

    let n = changed.load(Ordering::Relaxed);
    eprintln!(
        "Formatted {} of {} file{} in {:.2}s",
        n,
        files.len(),
        if files.len() == 1 { "" } else { "s" },
        start.elapsed().as_secs_f64()
    );

    if mode != Mode::Write && n > 0 {
        std::process::exit(1);
    }
}

/// Format one file according to `mode`. Returns whether it changed (or would).
fn format_one(path: &Path, mode: Mode, config: &Config, full: bool) -> Result<bool, String> {
    let ext = path.extension().and_then(|s| s.to_str()).unwrap_or("");
    let original = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    let formatted = format::format_source_with(&original, ext, config, full);

    if formatted == original {
        return Ok(false);
    }

    match mode {
        Mode::Write => {
            std::fs::write(path, &formatted).map_err(|e| e.to_string())?;
            eprintln!("Formatted: {}", path.display());
        }
        Mode::Check => {
            eprintln!("Would format: {}", path.display());
        }
        Mode::Diff => {
            print_diff(&path.display().to_string(), &original, &formatted);
        }
    }
    Ok(true)
}

fn run_stdin(ext: &str, config: &Config, full: bool) {
    let mut input = String::new();
    if let Err(e) = std::io::stdin().read_to_string(&mut input) {
        eprintln!("Error reading stdin: {}", e);
        std::process::exit(1);
    }
    let ext = ext.trim_start_matches('.');
    if config.skip_extensions.iter().any(|s| s == ext) {
        print!("{}", input);
        return;
    }
    print!("{}", format::format_source_with(&input, ext, config, full));
}

fn collect_targets(targets: &[String], config: &Config) -> Vec<PathBuf> {
    let mut files = Vec::new();
    for target in targets {
        let path = Path::new(target);
        if path.is_dir() {
            if let Err(e) = discovery::collect(path, &mut files, config) {
                eprintln!("Error reading directory {}: {}", target, e);
            }
        } else if path.exists() {
            files.push(path.to_path_buf());
        } else {
            match glob::glob(target) {
                Ok(paths) => {
                    for entry in paths.flatten() {
                        if !discovery::is_in_skipped_dir(&entry, config) {
                            files.push(entry);
                        }
                    }
                }
                Err(e) => eprintln!("Invalid glob {}: {}", target, e),
            }
        }
    }
    files
}

/// Files with uncommitted changes (modified/added/untracked), filtered to
/// formattable paths. Ported from reefmt.
fn git_changed_files(config: &Config) -> Vec<PathBuf> {
    let cwd = match std::env::current_dir() {
        Ok(c) => c,
        Err(_) => return vec![],
    };
    let out = Command::new("git").args(["status", "--porcelain"]).output();
    let mut files = Vec::new();
    match out {
        Ok(o) if o.status.success() => {
            for line in String::from_utf8_lossy(&o.stdout).lines() {
                if line.len() < 4 {
                    continue;
                }
                let chars: Vec<char> = line[..2].chars().collect();
                if chars[0] == 'D' || chars[1] == 'D' {
                    continue;
                }
                let raw = &line[3..];
                let rel = raw.rfind(" -> ").map(|p| &raw[p + 4..]).unwrap_or(raw);
                let rel = rel.trim().trim_matches('"');
                let path = cwd.join(rel);
                if path.exists()
                    && !discovery::is_in_skipped_dir(&path, config)
                    && discovery::is_formattable(&path, config)
                {
                    files.push(path);
                }
            }
        }
        _ => eprintln!("Warning: not a git repository, --git has no effect"),
    }
    files
}

fn print_diff(name: &str, old: &str, new: &str) {
    use similar::{ChangeTag, TextDiff};
    println!("--- {}", name);
    println!("+++ {}", name);
    let diff = TextDiff::from_lines(old, new);
    for change in diff.iter_all_changes() {
        let sign = match change.tag() {
            ChangeTag::Delete => "-",
            ChangeTag::Insert => "+",
            ChangeTag::Equal => " ",
        };
        print!("{}{}", sign, change);
    }
}

fn print_help() {
    println!("reettier {} — layout-preserving formatter for .ree/.ts/.js/.css", env!("CARGO_PKG_VERSION"));
    println!();
    println!("USAGE:");
    println!("  reettier [OPTIONS] [PATH...]");
    println!();
    println!("OPTIONS:");
    println!("  --full                   Reprint: re-derive layout from the syntax tree");
    println!("                           (default is the layout-preserving indenter)");
    println!("  --check, -c, --dry-run   List files that would change (exit 1 if any)");
    println!("  --diff                   Show a unified diff without writing");
    println!("  --git                    Format only uncommitted (git-changed) files");
    println!("  --verbose                Also print already-formatted files");
    println!("  --stdin [.ext]           Read stdin, write stdout (ext defaults to .ree)");
    println!("  --init                   Create a starter reettier.jsonc in this directory");
    println!("  --version, -v            Print the bare version number");
    println!("  --help, -h               Print this help");
    println!();
    println!("CONFIG: reettier.jsonc in the current directory (optional).");
    println!("        --full knobs live under the \"full\" block; see --init output.");
}

/// Scaffold a starter `reettier.jsonc` in the current directory. Config is
/// optional, so this only helps discovery of the available keys - it never
/// overwrites an existing file.
fn run_init() {
    let cwd = std::env::current_dir().unwrap_or_else(|e| {
        eprintln!("Error: cannot determine current directory: {}", e);
        std::process::exit(1);
    });
    let path = cwd.join("reettier.jsonc");
    if path.exists() {
        eprintln!("{} already exists - not overwriting.", path.display());
        std::process::exit(1);
    }
    let template = include_str!("../reettier.jsonc.template");
    match std::fs::write(&path, template) {
        Ok(_) => println!("Created: {}", path.display()),
        Err(e) => {
            eprintln!("Error writing {}: {}", path.display(), e);
            std::process::exit(1);
        }
    }
}
