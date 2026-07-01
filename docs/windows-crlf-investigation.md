# Windows handoff: `.ree` "not formatting" — CRLF line-ending investigation

**Audience:** an agent running **natively on Windows** with a checkout of this repo
and a Rust toolchain. macOS could not reproduce the platform-specific symptom, so
this is a diagnose-and-fix task that must be done on Windows.

**Symptom (reported by user):** this `.ree` snippet is **not formatted on Windows**
but **is formatted on macOS**:

```
{#each outcomes as item}
					<li>{~ md(item)}</li>				{/each}
```

(Note: `</li>` and `{/each}` are on the **same physical line**, separated by tabs.)

---

## What macOS already established (don't re-do this — build on it)

1. **Same-line closer is a red herring for the platform gap.** On macOS, a line like
   `…</li>\t\t\t\t{/each}` is left over-indented on **both** LF and CRLF input,
   because `max(structural, author)` keeps the author's larger indent
   (`indent_markup`, `src/ree.rs:236`). That behavior is identical on both
   platforms, so it is *not* the Windows/macOS difference. (It may still be a
   separate wishlist item — see "Out of scope" below.)

2. **Confirmed real CRLF defect: mixed line endings in output.** Feeding a CRLF
   `.ree` file that contains an embedded multi-line `<script>` produces output where
   the **markup lines keep `\r\n`** but the **embedded code lines are `\n` only**:

   ```
   0: CRLF  <ul>
   1: CRLF  \t{#each outcomes as item}
   2: CRLF  \t\t<li>{~ md(item)}</li>
   3: CRLF  \t{/each}
   4: CRLF  </ul>
   5: LF    <script>
   6: LF    \tconst a = [
   7: LF    \t\t1,
   8: LF    \t\t2,
   9: LF    \t]
   10: CRLF  </script>
   ```

   Root cause of the split: the markup path splits on `'\n'` and keeps the trailing
   `\r` (`indent_markup`, `src/ree.rs:213`), while the embedded-code path runs
   through `str::lines()` — which **strips `\r`** — and re-joins bodies with `\n`
   (`dedent` `src/ree.rs:474`, `code_block` `src/ree.rs:166`, `render_line`
   `src/ree.rs:266`). This mixed output is a bug regardless of the user's specific
   symptom and is very likely related.

The whitespace-insensitive safety net (`strip`, `src/ree.rs:41`) filters `\r` out,
so mixed endings do **not** trip it — the file still gets written, just with
inconsistent endings.

---

## Hypotheses, ranked (check in this order)

### H1 — Version skew (cheapest; check FIRST)
The Windows `reettier` may be an **older released binary** (installed via
`install.ps1`, which pulls the latest GitHub *release*), while macOS runs a freshly
built dev binary that already has the `.ree` markup + same-line fixes. If so, the
Windows binary simply predates the feature.

**Test:**
```powershell
reettier --version
# Compare against the version in Cargo.toml in this checkout:
Select-String -Path Cargo.toml -Pattern '^version'
```
If Windows is behind, `cargo build --release` from this checkout and test with the
freshly built `target\release\reettier.exe` before concluding anything about CRLF.

### H2 — CRLF trips a safety net → whole-file no-op (most likely true root cause)
"Not formatting" strongly implies the **entire file is emitted unchanged**. That is
exactly what happens when a safety net fires:
- `.ree` top-level: `format_ree` (`src/ree.rs:29`) emits `src` verbatim when
  `strip(src) != strip(out)`.
- embedded JS/CSS: `engine::format_js` / `format_css` have their own token-signature
  safety nets.

`strip` is `\r`-insensitive, so a *pure* markup file shouldn't no-op from `\r`
alone. But a **real** `.ree` file usually also contains `<script>` / `<style>` /
`<pre>` / `<!-- -->`. If any `\r`-sensitive path (block extraction boundaries in
`extract_blocks` `src/ree.rs:69`; `element_content` `src/ree.rs:578`; the JS/CSS
tokenizer) miscounts on CRLF, the sub-format can corrupt a token → the safety net
fires → **the whole file is left unchanged**, which the user perceives as "the
`{#each}` didn't format." This is the leading hypothesis for the actual symptom.

**Test (native Windows, real file):** run on the user's actual failing `.ree` with
debug tracing on:
```powershell
$env:REETTIER_DEBUG = "1"
reettier --diff path\to\failing.ree
```
If you see `reettier: .ree content mismatch — leaving file unchanged`, H2 is
confirmed — a CRLF-sensitive path corrupted the reformatted output.

### H3 — Mixed line endings (confirmed on macOS, see above)
Even when the file *does* change, embedded blocks come out LF while markup stays
CRLF. On Windows this shows up as git churn and editor "mixed EOL" warnings, and can
look like "it didn't format properly."

---

## Reproduce natively on Windows

Create CRLF fixtures (PowerShell 5 writes CRLF by default with `Set-Content`; be
explicit to be safe):

```powershell
# Minimal each/li/each, CRLF
$each = "<ul>`r`n{#each outcomes as item}`r`n<li>{~ md(item)}</li>`r`n{/each}`r`n</ul>`r`n"
[IO.File]::WriteAllText("$PWD\crlf_each.ree", $each)

# Same-line closer (exact user shape), CRLF
$same = "<ul>`r`n{#each outcomes as item}`r`n`t`t`t`t`t<li>{~ md(item)}</li>`t`t`t`t{/each}`r`n</ul>`r`n"
[IO.File]::WriteAllText("$PWD\crlf_same.ree", $same)

# each/li/each + embedded multi-line <script>, CRLF (mixed-ending repro)
$mix = "<ul>`r`n{#each outcomes as item}`r`n<li>{~ md(item)}</li>`r`n{/each}`r`n</ul>`r`n<script>`r`nconst a = [`r`n1,`r`n2`r`n]`r`n</script>`r`n"
[IO.File]::WriteAllText("$PWD\crlf_mix.ree", $mix)
```

Inspect exact bytes/endings of the output (don't trust the console — it may
normalize):

```powershell
cargo build --release
$exe = "$PWD\target\release\reettier.exe"

Get-Content crlf_mix.ree -Raw | & $exe --stdin .ree | Set-Content out.ree -NoNewline
# Dump per-line ending:
$bytes = [IO.File]::ReadAllBytes("$PWD\out.ree")
-join ($bytes | ForEach-Object { '{0:X2} ' -f $_ })   # hex dump; look for 0D 0A vs bare 0A
```

Also compare against LF: pipe the same content with `\n` endings and diff.

**Acceptance for "reproduced":** either (a) the debug line from H2 prints, or
(b) `out.ree` has mixed `0D 0A` / bare `0A` endings, or (c) a `--diff` run shows no
change on a file macOS *does* change.

---

## Recommended fix — normalize at the boundary, restore on the way out

Centralize all CRLF handling at the single dispatch choke point so none of the inner
logic ever sees `\r`. This kills H2 and H3 together and keeps the engine CRLF-free.

**Where:** `format_source` in `src/format.rs:9` (every path — ts/js/css/ree — flows
through it), plus the two I/O sites `format_one` (`src/main.rs:171`) and `run_stdin`
(`src/main.rs:193`).

**Approach:**
1. On input, detect the file's dominant convention: `let had_crlf = content.contains("\r\n");`
2. Normalize to LF before formatting: `let lf = content.replace("\r\n", "\n");`
   (Also consider a lone-`\r` (old-Mac) pass, but CRLF is the real-world case.)
3. Format the LF text through the existing engines (unchanged).
4. If `had_crlf`, convert back on output: `out.replace('\n', "\r\n")`.

Sketch for `format_source` (keeps the engines untouched):

```rust
pub fn format_source(content: &str, ext: &str, config: &Config) -> String {
    let had_crlf = content.contains("\r\n");
    let lf = if had_crlf { content.replace("\r\n", "\n") } else { content.to_string() };
    let out = match ext {
        "ts" | "js" => format_js(&lf, config),
        "css"       => format_css(&lf, config),
        "ree"       => format_ree(&lf, config),
        _           => return content.to_string(), // unknown ext: byte-for-byte passthrough
    };
    if had_crlf { out.replace('\n', "\r\n") } else { out }
}
```

Notes / gotchas:
- Do the `had_crlf` check on the **original** content, before normalization.
- The unknown-extension arm must return the **original** `content` (not `lf`) so
  non-formatted files are never touched.
- `format_one` (`src/main.rs:174`) compares `formatted == original` to decide
  "changed". After this fix a pure line-ending normalization (e.g. a file with
  mixed endings) will correctly count as a change — that's desired.
- `run_stdin` inherits the fix automatically since it calls `format_source`.
- Leave the engine/`ree.rs` internals alone; they now always operate on LF, so the
  `.lines()` vs `split('\n')` asymmetry becomes harmless.

---

## Verification checklist (run all on Windows before declaring done)

1. `cargo test` — all existing tests green (56+ expected).
2. **Add regression tests** to `src/ree.rs` `#[cfg(test)]`:
   - CRLF input round-trips to **all-CRLF** output (no bare `\n`), including a case
     with an embedded multi-line `<script>` (the confirmed mixed-ending case).
   - CRLF formatting is **idempotent** (`fmt(fmt(x)) == fmt(x)`).
   - A `.ts`/`.css` CRLF file likewise stays all-CRLF.
   (Assert on raw bytes / count `\r` vs `\n`, not on rendered text.)
3. Re-run the three fixtures above; confirm hex dump is **uniformly `0D 0A`**.
4. On the user's **actual failing file**, confirm it now formats (and `REETTIER_DEBUG`
   prints no mismatch line).
5. Run `reettier .` over a **real Windows checkout of apolee and reepolee-labs-eu**
   (`wip`/`reettier` branches): expect 0 safety-net firings, 0 non-idempotent files,
   and no spurious line-ending-only churn on files that were already all-CRLF.

---

## Out of scope for this task (log, don't fix here)

- The `max(structural, author)` over-indent on the same-line `…</li>{/each}` shape
  (`src/ree.rs:236`) — this is identical on macOS and is a separate design question
  (should a trailing block-closer force a dedent of its own segment?). Note it in
  `docs/edge-cases.md` if you create that file; do not change it as part of the CRLF
  fix.
- Attribute first-boundary explode/collapse (still unimplemented).

---

## Report back

State which hypothesis was the true root cause (H1/H2/H3), paste the before/after hex
dump of one fixture, the `cargo test` summary, and the apolee/reepolee full-run
counts. If H1 (version skew) turns out to be the whole story, say so plainly — the
CRLF normalization is still worth landing (H3 is real), but the user's immediate
symptom would be fixed by a rebuild/reinstall.
