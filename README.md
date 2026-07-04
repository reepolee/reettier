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

This is a Rust project.

```bash
cargo test
cargo build --release
```

To build and publish a release (bumps version, tags, uploads binaries, installs
locally):

```bash
bash release.sh          # macOS / Linux
.\release.ps1            # Windows
```

On any branch other than `main`, `release.sh`/`release.ps1` instead builds a local
dev binary named `reettier-<branch>` (no publish), so you can test a branch build
side-by-side with the released `reettier`. Pass `--release-branch` / `-ReleaseBranch`
to force a real publish from a branch.
