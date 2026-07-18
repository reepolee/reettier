//! Lossless, context-aware tokenizer for JS/TS.
//!
//! This is the foundation of the whole engine (ADR-0001): the four rules need
//! only bracket/comma structure, so the tokenizer's one hard job is to scan
//! string / template / regex / comment spans correctly, so that brackets and
//! commas *inside* them are never mistaken for structure. Templates (including
//! their `${…}` substitutions) are treated as a single opaque span — reettier
//! never reflows template content.
//!
//! Invariant: concatenating every token's `text` in order reproduces the input
//! exactly (losslessness). Structural delimiters are all ASCII, so every token
//! boundary lands on a UTF-8 char boundary.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokKind {
    /// One line break (`\n` or `\r\n`).
    Newline,
    /// A run of spaces/tabs (no line break).
    Space,
    /// `// …` up to (not including) the line break.
    LineComment,
    /// `/* … */`.
    BlockComment,
    /// A single- or double-quoted string literal.
    Str,
    /// A whole template literal, `${…}` substitutions included (opaque).
    Template,
    /// A regular-expression literal.
    Regex,
    /// An opening bracket: `(`, `[`, or `{`.
    Open,
    /// A closing bracket: `)`, `]`, or `}`.
    Close,
    /// `,`
    Comma,
    /// `;`
    Semicolon,
    /// An identifier / keyword / numeric run.
    Word,
    /// Any other operator or punctuation run (`=`, `=>`, `.`, `<`, `&&`, …).
    Punct,
}

#[derive(Debug, Clone)]
pub struct Token {
    pub kind: TokKind,
    pub start: usize,
    pub end: usize,
}

impl Token {
    pub fn text<'a>(&self, src: &'a str) -> &'a str {
        &src[self.start..self.end]
    }
}

fn is_ident_start(b: u8) -> bool {
    b.is_ascii_alphabetic() || b == b'_' || b == b'$' || b >= 0x80
}
fn is_ident_continue(b: u8) -> bool {
    is_ident_start(b) || b.is_ascii_digit()
}

/// Keywords after which a `/` begins a regex, not a division.
const REGEX_PRECEDING_KEYWORDS: &[&str] = &[
    "return", "typeof", "instanceof", "in", "of", "new", "delete", "void", "do",
    "else", "yield", "await", "case",
];

/// Whether a `/` at this point starts a regex, based on the previous
/// significant token. A regex can follow operators, `(`, `,`, `{`, `[`, `;`,
/// `=>`, or an expression keyword; a `/` after a value (identifier, `)`, `]`,
/// number, string, template) is division.
fn regex_allowed(prev: Option<(TokKind, &str)>) -> bool {
    match prev {
        None => true,
        Some((kind, text)) => match kind {
            TokKind::Word => REGEX_PRECEDING_KEYWORDS.contains(&text),
            TokKind::Str | TokKind::Template | TokKind::Regex => false,
            TokKind::Close => false, // )  ]  } → value → division
            TokKind::Punct | TokKind::Comma | TokKind::Semicolon | TokKind::Open => true,
            // Comments/whitespace are skipped before calling this.
            _ => true,
        },
    }
}

/// The "meaning-bearing" token stream: significant tokens minus whitespace and
/// commas (commas are managed by the formatter as trailing-comma shape). Used by
/// the self-verify safety net to prove a format didn't drop or reorder code.
pub fn signature(src: &str, css: bool) -> Vec<(TokKind, &str)> {
    let toks = if css { tokenize_css(src) } else { tokenize(src) };
    toks.into_iter()
        .filter(|t| !matches!(t.kind, TokKind::Space | TokKind::Newline | TokKind::Comma))
        .map(|t| (t.kind, &src[t.start..t.end]))
        .collect()
}

/// Collapse redundant statement-level semicolons in a signature stream: empty
/// statements and `;;` runs. A statement-level `;` is redundant when the
/// previous kept significant token is another `;`, a block-opening `{`, or the
/// start of input. Semicolons inside `(…)` (for-headers) or `[…]` are never
/// touched. The formatter drops exactly these, so the self-verify runs both its
/// reference (source) and result through this before comparing — otherwise the
/// intentional removal would look like token loss and revert the file.
pub fn strip_redundant_semicolons<'a>(sig: &[(TokKind, &'a str)]) -> Vec<(TokKind, &'a str)> {
    let mut out: Vec<(TokKind, &'a str)> = Vec::with_capacity(sig.len());
    let mut stack: Vec<u8> = Vec::new();
    for &(kind, text) in sig {
        match kind {
            TokKind::Open => {
                stack.push(text.as_bytes()[0]);
                out.push((kind, text));
            }
            TokKind::Close => {
                stack.pop();
                out.push((kind, text));
            }
            TokKind::Semicolon => {
                let in_paren_bracket = matches!(stack.last(), Some(b'(') | Some(b'['));
                let redundant = !in_paren_bracket
                    && match out.last() {
                        None => true,
                        Some(&(TokKind::Semicolon, _)) => true,
                        Some(&(TokKind::Open, t)) => t.as_bytes()[0] == b'{',
                        _ => false,
                    };
                if !redundant {
                    out.push((kind, text));
                }
            }
            _ => out.push((kind, text)),
        }
    }
    out
}

pub fn tokenize(src: &str) -> Vec<Token> {
    tokenize_impl(src, false)
}

/// CSS flavor: no `//` line comments, no regex, no template literals — a `/`
/// that isn't `/*` is ordinary punctuation, and backticks are punctuation.
pub fn tokenize_css(src: &str) -> Vec<Token> {
    tokenize_impl(src, true)
}

fn tokenize_impl(src: &str, css: bool) -> Vec<Token> {
    let b = src.as_bytes();
    let n = b.len();
    let mut i = 0;
    let mut tokens: Vec<Token> = Vec::new();
    // Last significant (non-trivia) token, for regex disambiguation.
    let mut prev_sig: Option<(TokKind, (usize, usize))> = None;

    while i < n {
        let start = i;
        let c = b[i];

        // ── Line breaks ──
        if c == b'\n' {
            i += 1;
            tokens.push(Token { kind: TokKind::Newline, start, end: i });
            continue;
        }
        if c == b'\r' {
            i += 1;
            if i < n && b[i] == b'\n' {
                i += 1;
            }
            tokens.push(Token { kind: TokKind::Newline, start, end: i });
            continue;
        }

        // ── Horizontal whitespace ──
        if c == b' ' || c == b'\t' || c == 0x0c || c == 0x0b {
            while i < n && (b[i] == b' ' || b[i] == b'\t' || b[i] == 0x0c || b[i] == 0x0b) {
                i += 1;
            }
            tokens.push(Token { kind: TokKind::Space, start, end: i });
            continue;
        }

        // ── Comments ──
        if !css && c == b'/' && i + 1 < n && b[i + 1] == b'/' {
            i += 2;
            while i < n && b[i] != b'\n' && b[i] != b'\r' {
                i += 1;
            }
            let t = Token { kind: TokKind::LineComment, start, end: i };
            prev_sig = Some((t.kind, (t.start, t.end)));
            tokens.push(t);
            continue;
        }
        if c == b'/' && i + 1 < n && b[i + 1] == b'*' {
            i += 2;
            while i < n && !(b[i] == b'*' && i + 1 < n && b[i + 1] == b'/') {
                i += 1;
            }
            i = (i + 2).min(n); // consume closing */
            let t = Token { kind: TokKind::BlockComment, start, end: i };
            prev_sig = Some((t.kind, (t.start, t.end)));
            tokens.push(t);
            continue;
        }

        // ── Strings ──
        if c == b'\'' || c == b'"' {
            i = scan_string(b, i, c);
            let t = Token { kind: TokKind::Str, start, end: i };
            prev_sig = Some((t.kind, (t.start, t.end)));
            tokens.push(t);
            continue;
        }

        // ── Template literals (opaque, incl. ${…}) ──
        if !css && c == b'`' {
            i = scan_template(b, i);
            let t = Token { kind: TokKind::Template, start, end: i };
            prev_sig = Some((t.kind, (t.start, t.end)));
            tokens.push(t);
            continue;
        }

        // ── Regex vs division ──
        if !css && c == b'/' {
            let prev = prev_sig.map(|(k, (s, e))| (k, &src[s..e]));
            if regex_allowed(prev) {
                if let Some(end) = scan_regex(b, i) {
                    i = end;
                    let t = Token { kind: TokKind::Regex, start, end: i };
                    prev_sig = Some((t.kind, (t.start, t.end)));
                    tokens.push(t);
                    continue;
                }
            }
            // Fall through: treat `/` as punctuation (division).
        }

        // ── Brackets / comma / semicolon ──
        let single = match c {
            b'(' | b'[' | b'{' => Some(TokKind::Open),
            b')' | b']' | b'}' => Some(TokKind::Close),
            b',' => Some(TokKind::Comma),
            b';' => Some(TokKind::Semicolon),
            _ => None,
        };
        if let Some(kind) = single {
            i += 1;
            let t = Token { kind, start, end: i };
            prev_sig = Some((t.kind, (t.start, t.end)));
            tokens.push(t);
            continue;
        }

        // ── Identifier / keyword / number run ──
        if is_ident_start(c) || c.is_ascii_digit() {
            while i < n && is_ident_continue(b[i]) {
                i += 1;
            }
            let t = Token { kind: TokKind::Word, start, end: i };
            prev_sig = Some((t.kind, (t.start, t.end)));
            tokens.push(t);
            continue;
        }

        // ── Operator / other punctuation run ──
        // Consume a run of punctuation bytes that aren't handled above, so we
        // don't emit one token per char for things like `===` or `=>`.
        while i < n {
            let d = b[i];
            let stop = d == b'\n'
                || d == b'\r'
                || d == b' '
                || d == b'\t'
                || d == 0x0c
                || d == 0x0b
                || d == b'\''
                || d == b'"'
                || d == b'`'
                || d == b'('
                || d == b')'
                || d == b'['
                || d == b']'
                || d == b'{'
                || d == b'}'
                || d == b','
                || d == b';'
                || d == b'/'
                || is_ident_start(d)
                || d.is_ascii_digit();
            if stop {
                break;
            }
            i += 1;
        }
        if i == start {
            // Safety: guarantee progress (e.g. a lone `/` that wasn't a regex).
            i += 1;
        }
        let t = Token { kind: TokKind::Punct, start, end: i };
        prev_sig = Some((t.kind, (t.start, t.end)));
        tokens.push(t);
    }

    tokens
}

/// Scan a quoted string starting at `i` (byte is the quote). Returns index past
/// the closing quote (or end of input on an unterminated string).
fn scan_string(b: &[u8], mut i: usize, quote: u8) -> usize {
    let n = b.len();
    i += 1; // opening quote
    while i < n {
        match b[i] {
            b'\\' => i += 2,
            x if x == quote => return i + 1,
            b'\n' | b'\r' => return i, // unterminated (single-line) string
            _ => i += 1,
        }
    }
    n
}

/// Scan a template literal starting at the backtick. Handles escapes, nested
/// `${…}` substitutions (with their own strings/templates/comments), and nested
/// templates. Returns index past the closing backtick.
fn scan_template(b: &[u8], mut i: usize) -> usize {
    let n = b.len();
    i += 1; // opening backtick
    while i < n {
        match b[i] {
            b'\\' => i += 2,
            b'`' => return i + 1,
            b'$' if i + 1 < n && b[i + 1] == b'{' => {
                i = scan_template_expr(b, i + 2);
            }
            _ => i += 1,
        }
    }
    n
}

/// Whether a `/` in this byte-context begins a regex (not division), based on
/// the previous significant byte. Mirrors `regex_allowed` at the byte level:
/// division follows a value (identifier/number/`)`/`]`/`}`/quote/backtick).
fn regex_allowed_byte(prev: u8) -> bool {
    !(prev.is_ascii_alphanumeric()
        || prev == b'_'
        || prev == b'$'
        || prev >= 0x80
        || matches!(prev, b')' | b']' | b'}' | b'"' | b'\'' | b'`'))
}

/// Scan a `${ … }` substitution interior starting just after `${`. Balances
/// braces while skipping nested string/template/regex/comment spans. Regex
/// handling is essential: a regex literal such as `` /`/g `` contains a
/// backtick that must not be mistaken for a nested template (which would
/// swallow the rest of the file). Returns index past the matching `}`.
fn scan_template_expr(b: &[u8], mut i: usize) -> usize {
    let n = b.len();
    let mut depth: usize = 1;
    // Position just after `${` is an expression start → a `/` there is a regex.
    let mut prev: u8 = b'{';
    while i < n {
        let c = b[i];
        match c {
            b'{' => {
                depth += 1;
                i += 1;
                prev = c;
            }
            b'}' => {
                depth -= 1;
                i += 1;
                if depth == 0 {
                    return i;
                }
                prev = c;
            }
            b'\'' | b'"' => {
                i = scan_string(b, i, c);
                prev = c;
            }
            b'`' => {
                i = scan_template(b, i);
                prev = b'`';
            }
            b'/' if i + 1 < n && b[i + 1] == b'/' => {
                i += 2;
                while i < n && b[i] != b'\n' && b[i] != b'\r' {
                    i += 1;
                }
                // comment is transparent — leave `prev` unchanged
            }
            b'/' if i + 1 < n && b[i + 1] == b'*' => {
                i += 2;
                while i < n && !(b[i] == b'*' && i + 1 < n && b[i + 1] == b'/') {
                    i += 1;
                }
                i = (i + 2).min(n);
            }
            b'/' if regex_allowed_byte(prev) => {
                if let Some(end) = scan_regex(b, i) {
                    i = end;
                    prev = b'/';
                } else {
                    i += 1;
                    prev = c;
                }
            }
            b' ' | b'\t' | b'\n' | b'\r' => i += 1, // whitespace — keep `prev`
            _ => {
                i += 1;
                prev = c;
            }
        }
    }
    n
}

/// Scan a regex literal starting at the leading `/`. Returns index past the
/// trailing flags, or `None` if it doesn't look like a valid single-line regex
/// (in which case the `/` is division).
fn scan_regex(b: &[u8], mut i: usize) -> Option<usize> {
    let n = b.len();
    i += 1; // leading /
    let mut in_class = false;
    while i < n {
        match b[i] {
            b'\\' => i += 2,
            b'\n' | b'\r' => return None, // regex can't span lines
            b'[' => {
                in_class = true;
                i += 1;
            }
            b']' => {
                in_class = false;
                i += 1;
            }
            b'/' if !in_class => {
                i += 1;
                // consume flags
                while i < n && is_ident_continue(b[i]) {
                    i += 1;
                }
                return Some(i);
            }
            _ => i += 1,
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Losslessness: rejoining token slices must reproduce the input.
    fn assert_lossless(src: &str) {
        let toks = tokenize(src);
        let joined: String = toks.iter().map(|t| t.text(src)).collect();
        assert_eq!(joined, src, "tokenizer lost bytes for: {:?}", src);
    }

    fn kinds(src: &str) -> Vec<TokKind> {
        tokenize(src).into_iter().map(|t| t.kind).collect()
    }

    #[test]
    fn lossless_variety() {
        for src in [
            "const x = 1;\n",
            "foo(a, b, c)\n",
            "let s = \"a, b, c\";\n",
            "let t = `hi ${name}, ${x + f(1, 2)}!`;\n",
            "const re = /a\\/b/gi;\n",
            "a / b / c\n",
            "// line, with, commas\nx\n",
            "/* block ; comma , */ y\n",
            "obj = { a: 1, b: [2, 3], c: (4) };\n",
            "arr[0]\n",
            "let π = 3; const café = '☕';\n",
            "x = a<b, c>d\n",
        ] {
            assert_lossless(src);
        }
    }

    #[test]
    fn brackets_and_commas_in_strings_are_not_structural() {
        let toks = tokenize(r#"f("a, (b) [c]")"#);
        // Expect: Word(f) Open Str Close  — no Comma/Open/Close from inside the string.
        let ks: Vec<_> = toks.iter().map(|t| t.kind).collect();
        assert_eq!(ks, vec![TokKind::Word, TokKind::Open, TokKind::Str, TokKind::Close]);
    }

    #[test]
    fn template_with_substitution_is_one_token() {
        let src = "`a ${ f(1, 2) } b ${ `nested ${x}` } c`";
        let toks = tokenize(src);
        assert_eq!(toks.len(), 1);
        assert_eq!(toks[0].kind, TokKind::Template);
        assert_eq!(toks[0].text(src), src);
    }

    #[test]
    fn regex_vs_division() {
        // Regex: after `=` and after `(`.
        assert!(kinds("x = /ab+c/;").contains(&TokKind::Regex));
        assert!(kinds("if (/x/.test(s)) {}").contains(&TokKind::Regex));
        // Division: after an identifier and after `)`.
        assert!(!kinds("a / b").contains(&TokKind::Regex));
        assert!(!kinds("(a) / b").contains(&TokKind::Regex));
        // Regex after `return`.
        assert!(kinds("return /x/g;").contains(&TokKind::Regex));
    }

    #[test]
    fn regex_inside_template_substitution() {
        // Regression: a regex `/`/g` inside a `${…}` — its backtick must not be
        // mistaken for a nested template, which would swallow the closing `)`
        // and unbalance the whole file.
        let src = r#"const r = (await db`X ${db.unsafe("`" + t.replace(/`/g, "``") + "`")}`) as any[];"#;
        let toks = tokenize(src);
        // The template (incl. its ${…}) must be exactly one opaque token.
        let templates: Vec<_> = toks.iter().filter(|t| t.kind == TokKind::Template).collect();
        assert_eq!(templates.len(), 1, "template mis-scanned");
        // Brackets must balance: equal Open and Close.
        let opens = toks.iter().filter(|t| t.kind == TokKind::Open).count();
        let closes = toks.iter().filter(|t| t.kind == TokKind::Close).count();
        assert_eq!(opens, closes, "brackets unbalanced after template");
        assert_lossless(src);
    }

    #[test]
    fn regex_char_class_with_slash() {
        let src = r"/[/]/g";
        let toks = tokenize(src);
        assert_eq!(toks.len(), 1);
        assert_eq!(toks[0].kind, TokKind::Regex);
    }

    #[test]
    fn line_comment_stops_at_newline() {
        let src = "x // c\ny";
        let ks = kinds(src);
        assert_eq!(
            ks,
            vec![
                TokKind::Word,     // x
                TokKind::Space,
                TokKind::LineComment,
                TokKind::Newline,
                TokKind::Word, // y
            ]
        );
        assert_lossless(src);
    }

    #[test]
    fn multibyte_identifiers_not_split() {
        assert_lossless("const café = 1; let 日本 = 2;");
    }

    #[test]
    fn operator_runs_are_single_tokens() {
        let toks = tokenize("a===b=>c");
        let texts: Vec<_> = toks.iter().map(|t| t.text("a===b=>c")).collect();
        assert!(texts.contains(&"==="));
        assert!(texts.contains(&"=>"));
    }

    #[test]
    fn crlf_is_one_newline() {
        let toks = tokenize("a\r\nb");
        let nl: Vec<_> = toks.iter().filter(|t| t.kind == TokKind::Newline).collect();
        assert_eq!(nl.len(), 1);
        assert_eq!(nl[0].text("a\r\nb"), "\r\n");
    }
}
