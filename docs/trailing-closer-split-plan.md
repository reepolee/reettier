# Plan: split trailing block-closers onto their own line

## Goal

Make reettier pull a **trailing structural block-closer** off the end of a line and
onto its own line, dedented one level. This fixes the symptom:

```ree
			<ul class="mt-6 flex flex-col gap-y-3 text-lg lg:text-xl">
				{#each outcomes as item}
					<li>{~ md(item)}</li>{/each}
			</ul>
```

should format to:

```ree
			<ul class="mt-6 flex flex-col gap-y-3 text-lg lg:text-xl">
				{#each outcomes as item}
					<li>{~ md(item)}</li>
				{/each}
			</ul>
```

Scope (confirmed): applies to **ree closers** (`{/each}`, `{/if}`, `{/with}`,
`{/for}`) **and trailing HTML block closers** (`</div>`, `</section>`, … any
non-void `</tag>`) that close a block opened on an **earlier** line.

This is consistent with the formatter's core goal (correct indentation), not a
violation of "never split lines": we only peel a **structural closer** that was
never matched by an opener on the same line. Sibling content like
`<li>a</li><li>b</li>` and fully balanced same-line groups like
`<div>…<h2>x</h2>…</div>` are left untouched because every closer there is
matched by a same-line opener.

All logic changes are in `src/ree.rs`. No engine or CRLF code is touched. The
`strip()` safety net still passes (splitting only changes whitespace).

---

## Step 1 — add `split_trailing_closers` to `src/ree.rs`

Place this new function next to `analyze_line`. It reuses the existing helpers
`tag_end`, `tag_name`, `brace_end`, `ree_keyword`, `utf8_len`, and the consts
`VOID`, `REE_BLOCK_OPEN`.

```rust
/// Trailing structural block-closers on a line: ree `{/kw}` or non-void HTML
/// `</tag>` closers that are **not** matched by an opener earlier on the same
/// line (they close a block opened on a previous line) and sit at the **tail**
/// of the line (only whitespace / further such closers follow).
///
/// Returns `(head, closers)`: `head` is the content before the trailing run,
/// `closers` each trailing closer in source order. `closers` is empty when
/// there is nothing to split — no trailing structural closer, or no
/// non-whitespace content precedes it (a lone closer, which the leading-closer
/// path already handles correctly).
fn split_trailing_closers(line: &str) -> (&str, Vec<&str>) {
    let b = line.as_bytes();
    let n = b.len();
    let mut i = 0usize;
    let mut local_depth: i32 = 0; // same-line openers still open
    let mut run_start: Option<usize> = None;
    let mut closers: Vec<(usize, usize)> = Vec::new();

    while i < n {
        let c = b[i];

        // Whitespace never breaks a trailing run.
        if c == b' ' || c == b'\t' {
            i += 1;
            continue;
        }

        // HTML close tag `</name>`
        if c == b'<' && i + 1 < n && b[i + 1] == b'/' {
            let end = tag_end(line, i);
            let name = tag_name(&line[i..end]);
            if VOID.contains(&name.as_str()) {
                run_start = None;
                closers.clear();
            } else if local_depth > 0 {
                // Matched by a same-line opener → content, breaks the run.
                local_depth -= 1;
                run_start = None;
                closers.clear();
            } else {
                // Structural closer (opened on a previous line).
                if run_start.is_none() {
                    run_start = Some(i);
                }
                closers.push((i, end));
            }
            i = end;
            continue;
        }

        // HTML open tag `<name …>`
        if c == b'<' && i + 1 < n && b[i + 1].is_ascii_alphabetic() {
            let end = tag_end(line, i);
            let closed = end > i && b[end - 1] == b'>';
            let name = tag_name(&line[i..end]);
            if closed {
                let self_closing = line[i..end].trim_end().ends_with("/>");
                if !self_closing && !VOID.contains(&name.as_str()) {
                    local_depth += 1;
                }
            }
            run_start = None;
            closers.clear();
            i = end;
            continue;
        }

        // Ree directive `{…}`
        if c == b'{' && i + 1 < n {
            match b[i + 1] {
                b'#' => {
                    let kw = ree_keyword(&line[i..]);
                    if REE_BLOCK_OPEN.contains(&kw.as_str()) {
                        local_depth += 1;
                    }
                    run_start = None;
                    closers.clear();
                    i = brace_end(line, i);
                    continue;
                }
                b'/' => {
                    let end = brace_end(line, i);
                    if local_depth > 0 {
                        local_depth -= 1;
                        run_start = None;
                        closers.clear();
                    } else {
                        if run_start.is_none() {
                            run_start = Some(i);
                        }
                        closers.push((i, end));
                    }
                    i = end;
                    continue;
                }
                // `{~ x}`, `{:else}`, `{ text }` — content, breaks the run.
                _ => {
                    run_start = None;
                    closers.clear();
                    i = brace_end(line, i);
                    continue;
                }
            }
        }

        // Any other visible char is content → breaks a trailing run.
        run_start = None;
        closers.clear();
        i += utf8_len(c);
    }

    match run_start {
        Some(s) => {
            let head = line[..s].trim_end();
            if head.is_empty() {
                // Lone closer(s): defer to the existing leading-closer path.
                (line, Vec::new())
            } else {
                let slices = closers.iter().map(|&(a, e)| line[a..e].trim()).collect();
                (head, slices)
            }
        }
        None => (line, Vec::new()),
    }
}
```

---

## Step 2 — wire it into `indent_markup`

Replace the current `else` branch in `indent_markup` (the normal-line case,
around `src/ree.rs:234`–`246`):

```rust
        } else {
            let (leading_closers, opens, closes, pending) = analyze_line(trimmed);
            let line_level = (depth - leading_closers as i32).max(0) as usize;
            let level = line_level.max(author);
            let base = indent.repeat(level);
            render_line(&mut out, trimmed, &base, indent, blocks);
            depth = (depth + opens as i32 - closes as i32).max(0);
            if let Some(is_opener) = pending {
                in_tag = true;
                tag_base = level;
                tag_opener = is_opener;
            }
        }
```

with:

```rust
        } else {
            let (head, trailing) = split_trailing_closers(trimmed);
            // Analyze the *head* only. When there is nothing to split,
            // `head == trimmed`, so this is identical to the old behaviour.
            let (leading_closers, opens, closes, pending) = analyze_line(head);
            let line_level = (depth - leading_closers as i32).max(0) as usize;
            let level = line_level.max(author);
            let base = indent.repeat(level);

            if trailing.is_empty() || pending.is_some() {
                // Unchanged path: render the whole line as one unit.
                render_line(&mut out, trimmed, &base, indent, blocks);
                depth = (depth + opens as i32 - closes as i32).max(0);
                if let Some(is_opener) = pending {
                    in_tag = true;
                    tag_base = level;
                    tag_opener = is_opener;
                }
            } else {
                // Render the content head, then peel each trailing block-closer
                // onto its own line, dedenting one level per closer relative to
                // the head's rendered level.
                render_line(&mut out, head, &base, indent, blocks);
                depth = (depth + opens as i32 - closes as i32).max(0);
                for (k, closer) in trailing.iter().enumerate() {
                    let closer_level = level.saturating_sub(1 + k);
                    let cbase = indent.repeat(closer_level);
                    out.push('\n');
                    render_line(&mut out, closer, &cbase, indent, blocks);
                    depth = (depth - 1).max(0);
                }
            }
        }
```

### Why the closer level is `level.saturating_sub(1 + k)` (not `depth - 1`)

The closer must align with where its **matching opener was rendered**. In a
document formatted from the root, that equals `depth - 1`; but when a line's
author indent exceeds its structural depth (the `max(structural, author)`
over-indent case), `depth` lags behind the rendered level. Anchoring each closer
to the head's **rendered** `level` (minus one per closer, staircase) keeps the
closer visually aligned with its opener in both cases. `depth` is still
decremented structurally so following lines stay correct.

---

## Step 3 — add tests to the `#[cfg(test)] mod tests` in `src/ree.rs`

```rust
    #[test]
    fn trailing_each_closer_splits_and_dedents() {
        let input = "<ul>\n{#each xs as x}\n<li>{~ md(x)}</li>{/each}\n</ul>\n";
        let expected = "<ul>\n\t{#each xs as x}\n\t\t<li>{~ md(x)}</li>\n\t{/each}\n</ul>\n";
        assert_eq!(fmt(input), expected);
    }

    #[test]
    fn trailing_closer_is_idempotent() {
        let once = fmt("<ul>\n{#each xs as x}\n<li>a</li>{/each}\n</ul>\n");
        assert_eq!(fmt(&once), once);
    }

    #[test]
    fn sibling_elements_not_split() {
        // Both closers matched by same-line openers — stays one line.
        let input = "<ul>\n<li>a</li><li>b</li>\n</ul>\n";
        assert_eq!(fmt(input), "<ul>\n\t<li>a</li><li>b</li>\n</ul>\n");
    }

    #[test]
    fn balanced_same_line_group_not_split() {
        // Every closer matched on the line → nothing structural to peel.
        let input = "<div>\n<div class=\"m\"><h2>x</h2><p>y</p></div>\n</div>\n";
        assert_eq!(fmt(input), "<div>\n\t<div class=\"m\"><h2>x</h2><p>y</p></div>\n</div>\n");
    }

    #[test]
    fn stacked_trailing_closers_staircase() {
        let input = "<section>\n{#each xs as x}\n{#if x}\n<li>a</li>{/if}{/each}\n</section>\n";
        let expected = "<section>\n\t{#each xs as x}\n\t\t{#if x}\n\t\t\t<li>a</li>\n\t\t{/if}\n\t{/each}\n</section>\n";
        assert_eq!(fmt(input), expected);
    }

    #[test]
    fn trailing_html_block_closer_splits() {
        let input = "<section>\n<div>\n<span>x</span></div>\n</section>\n";
        let expected = "<section>\n\t<div>\n\t\t<span>x</span>\n\t</div>\n</section>\n";
        assert_eq!(fmt(input), expected);
    }

    #[test]
    fn lone_closer_unchanged() {
        // No content before the closer → existing leading-closer path handles it.
        let input = "{#if a}\n<p>1</p>\n{/if}\n";
        assert_eq!(fmt(input), "{#if a}\n\t<p>1</p>\n{/if}\n");
    }
```

---

## Step 4 — verify (needs the Rust toolchain)

```bash
cd reettier
cargo test                 # all prior tests + the 7 new ones green
cargo build --release
./target/release/reettier --diff ../reepolee-labs-eu/src/components/case-study-article.ree
```

Expect the `--diff` to now show the `{/each}` moved onto its own line at the
`{#each}` indent. Then confirm idempotence over the repo:

```bash
./target/release/reettier ../reepolee-labs-eu
./target/release/reettier ../reepolee-labs-eu   # second run: 0 files changed
```

Acceptance: `case-study-article.ree` formats as shown at the top; the whole repo
is idempotent; no prior test regresses.

---

## Notes / edge cases already handled

- **Placeholder chars** (U+E000/E001 from masked `<script>`/comments) are treated
  as opaque content — they break a trailing run, so a masked block on the line
  never triggers a bad split.
- **Void / self-closing** tags don't open or close a block, so `<br>` / `<img/>`
  before a closer don't confuse `local_depth`.
- **Content after a closer** (`{/each}<span>x</span>`) disables the split — only a
  closer at the true tail of the line is peeled.
- **Broken multi-line open tag** on the same line (`pending.is_some()`) falls back
  to the old whole-line render, so attribute-continuation logic is unaffected.
- **`depth` bookkeeping** is unchanged in aggregate: head net + one decrement per
  peeled closer equals the old whole-line net, so lines after the split keep the
  correct structural depth.

---

## Related: CRLF mixed-ending fix (already applied to `src/format.rs`)

Separately, `format_source` in `src/format.rs` was updated to normalize CRLF→LF
before formatting and restore CRLF after, fixing mixed line endings in embedded
code blocks on Windows checkouts. Seven CRLF regression tests were added to
`src/format.rs`. Run `cargo test` to confirm those too. See
`docs/windows-crlf-investigation.md` for background.
```

