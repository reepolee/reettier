# reettier

A **layout-preserving** formatter for Ree Templates (`.ree`) and their embedded
JavaScript, TypeScript, and CSS — "(p)re(e)ttier".

Unlike a reflow formatter, reettier does **not** wrap code to a target width. The
author's own line breaks are the source of truth; reettier only fixes indentation,
punctuation spacing, and group shape. You steer the layout — reettier keeps it tidy.

## The four rules

1. **Never auto-break** — reettier won't introduce line breaks on its own.
2. **Correct indentation** for HTML/CSS/JS nesting.
3. **Trim & space** — punctuation-level spacing; collapse blank runs to one.
4. **Group switch** — a group explodes one-per-line when you break after its first
   element, and collapses otherwise. A group is a **comma** sequence in `(…)`/`[…]`/`{…}`,
   a **semicolon** member list in a type/interface literal (`{ a: string; b: number }`),
   or an HTML attribute list. Trailing commas are managed by shape: arrays `[…]` and
   objects `{…}` get them when exploded; function calls `(…)`, type/interface members,
   and CSS never do. Statement blocks and multi-declarators (`let a = 1, b = 2`) are
   never groups.

It's a pure Rust binary with zero runtime dependencies — no Node.js required. It
self-verifies: if a format wouldn't preserve the meaning-bearing tokens, the file
is left unchanged, so it can never corrupt your code.

## Install

**macOS / Linux:**

```bash
curl -fsSL https://raw.githubusercontent.com/reepolee/reettier/main/install.sh | bash
```

**Windows:**

```powershell
irm https://raw.githubusercontent.com/reepolee/reettier/main/install.ps1 | iex
```

Or download a binary from the [latest release](https://github.com/reepolee/reettier/releases/latest).

## Usage

```bash
reettier                       # format the current directory (recursive)
reettier path/to/file.ree      # a single file (.ree, .ts, .js, .css)
reettier "src/**/*.ts"         # a glob
cat file.ree | reettier --stdin        # stdin → stdout (defaults to .ree)
cat file.ts  | reettier --stdin .ts
```

### Flags

| Flag | Description |
|---|---|
| `--check`, `-c`, `--dry-run` | List files that would change; exit 1 if any (for CI). |
| `--diff` | Show a unified diff without writing. |
| `--git` | Format only uncommitted (git-changed) files. |
| `--verbose` | Also print already-formatted files. |
| `--stdin [.ext]` | Read stdin, write stdout (extension defaults to `.ree`). |
| `--version`, `-v` | Print the version. |
| `--help`, `-h` | Print usage. |

## Configuration

reettier needs no config file. Create an optional `reettier.jsonc` in your project
root to customize file discovery and the indent string:

```jsonc
{
	"skipDirs": ["node_modules", "vendor", "dist", "static", "templates"],
	"skipFiles": [],
	"skipExtensions": ["min.js", "min.css"],
	"extensions": ["ree", "ts", "js", "css"],
	"skipDotDirs": true,
	"indent": "\t"
}
```

## Development

This is a Rust project. To test locally without releasing:

```bash
cargo build --release
cp target/release/reettier ~/.local/bin/   # macOS/Linux
```

### Release workflow

Releases are cut from a single machine (the Mac mini). `release.sh` cross-builds
**all six targets** and publishes them as one GitHub Release:

- macOS arm64/x64 (native `cargo build`)
- Linux x64/arm64 (`cargo zigbuild`)
- Windows x64/arm64 (`cargo xwin build`)

```bash
bash release.sh            # bump, tag, cross-build all targets, publish
bash release.sh --minor    # bump the minor version instead of patch
bash release.sh --draft    # publish the release as a draft
```

One-time setup on the Mac:

```bash
brew install zig
cargo install cargo-zigbuild cargo-xwin
rustup target add x86_64-unknown-linux-gnu aarch64-unknown-linux-gnu x86_64-pc-windows-msvc aarch64-pc-windows-msvc
```
