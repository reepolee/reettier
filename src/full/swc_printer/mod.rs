//! Custom AST printer for TypeScript/JavaScript that outputs directly from SWC's AST.
//! Uses SWC for parsing, then walks the AST with a custom printer that handles
//! spacing, indentation correctly from the start.

mod stmt;
mod expr;
mod decl;
mod lit;
mod prop;
mod pat;
mod types;

use swc_core::ecma::ast::*;
use swc_core::common::comments::{SingleThreadedComments, Comments};
use swc_core::common::input::StringInput;
use swc_core::common::sync::Lrc;
use swc_core::common::{FileName, SourceMap, Spanned, BytePos};
use swc_core::ecma::parser::lexer::Lexer;
use swc_core::ecma::parser::{Parser, Syntax, TsSyntax};
use swc_core::ecma::ast::EsVersion;

pub(crate) struct Printer<'a> {
    buf: String,
    indent_level: usize,
    indent_str: String,
    comments: &'a SingleThreadedComments,
    cm: Lrc<SourceMap>,
    wrap_width: usize,
    collapse: crate::full::format::CollapseConfig,
}

impl<'a> Printer<'a> {
    pub fn new(indent_str: &str, wrap_width: usize, collapse: crate::full::format::CollapseConfig, comments: &'a SingleThreadedComments, cm: Lrc<SourceMap>) -> Self {
        Self {
            buf: String::with_capacity(4096),
            indent_level: 0,
            indent_str: indent_str.to_string(),
            comments,
            cm,
            wrap_width,
            collapse,
        }
    }

    pub fn print_module(mut self, module: &Module) -> String {
        if let Some(shebang) = &module.shebang {
            self.w("#!");
            self.w(&**shebang);
            self.nl();
        }
        for item in &module.body {
            self.print_module_item(item);
        }
        self.buf
    }

    // ─── helpers ────────────────────────────────────────────

    pub(super) fn w(&mut self, s: &str) { self.buf.push_str(s); }
    pub(super) fn nl(&mut self) { self.buf.push('\n'); }
    pub(super) fn indent(&mut self) { self.indent_level += 1; }
    pub(super) fn dedent(&mut self) { self.indent_level = self.indent_level.saturating_sub(1); }
    pub(super) fn wi(&mut self) {
        for _ in 0..self.indent_level {
            self.buf.push_str(&self.indent_str);
        }
    }

    /// Emit a double-quoted string literal, re-escaping the decoded value so
    /// that special characters (`"`, `\`, newlines, tabs, control chars) are
    /// encoded back into valid JS. Without this, strings like `"a\"b\nc"` would
    /// produce invalid output.
    pub(super) fn print_str_lit(&mut self, decoded: &str) {
        self.w("\"");
        self.w(&escape_str_content(decoded));
        self.w("\"");
    }

    /// Emit leading comments for a node at the given byte position.
    /// These are `// __REEFMT_*` placeholder lines that the preprocess step
    /// created to preserve block comments and blank lines through SWC formatting.
    pub(super) fn emit_leading_comments(&mut self, pos: BytePos) {
        if let Some(comments) = self.comments.get_leading(pos) {
            for c in &comments {
                match c.kind {
                    swc_core::common::comments::CommentKind::Line => {
                        self.wi();
                        self.w("//");
                        self.w(&c.text);
                        self.nl();
                    }
                    // Inline block comments (e.g. `catch { /* dead */ }`) are not
                    // masked by the preprocess step, so they reach the printer as
                    // real block comments. Emit them on their own line so they are
                    // never dropped.
                    swc_core::common::comments::CommentKind::Block => {
                        self.wi();
                        self.w("/*");
                        self.w(&c.text);
                        self.w("*/");
                        self.nl();
                    }
                }
            }
        }
    }

    /// Emit a single comment on its own indented line. Used for comments found
    /// inside an otherwise-empty block, where there is no statement to attach
    /// them to inline.
    pub(super) fn emit_inner_comment(&mut self, c: &swc_core::common::comments::Comment) {
        match c.kind {
            swc_core::common::comments::CommentKind::Line => {
                self.wi();
                self.w("//");
                self.w(&c.text);
                self.nl();
            }
            swc_core::common::comments::CommentKind::Block => {
                self.wi();
                self.w("/*");
                self.w(&c.text);
                self.w("*/");
                self.nl();
            }
        }
    }

    /// Emit trailing comments (inline) for a node at the given byte position.
    /// Used for block comments that appear inline after statements, like
    /// `const x = 1; /* inline */`.
    /// Measure the current line length (chars emitted since last \n).
    /// On-screen width of the line currently being built (the text after the
    /// last newline in the buffer). Tabs are expanded to `tab_width` columns so
    /// deeply indented structures are measured by where their last character
    /// lands, not by raw character count.
    pub(super) fn current_line_len(&self) -> usize {
        let line = self.buf.rsplit('\n').next().unwrap_or("");
        crate::full::format::display_width(line, self.collapse.tab_width)
    }

    /// Decide whether a just-emitted inline structure may stay on one line,
    /// using the "soft width overrides count" rule. `cap` is the per-category
    /// member limit for this structure (e.g. `max_call_args`).
    ///
    /// - The current line must never exceed the hard `wrap_width`.
    /// - Within the soft width, the structure collapses regardless of `count`.
    /// - Above the soft width, `count` must be within `cap`.
    ///
    /// Pass `usize::MAX` for `cap` to opt out of the count limit (used when
    /// collapsing is configured to ignore counts for a given structure).
    pub(super) fn inline_fits(&self, count: usize, cap: usize) -> bool {
        let width = self.current_line_len();
        if width > self.wrap_width {
            return false;
        }
        count <= cap || width <= self.collapse.soft_wrap_width
    }

    pub(super) fn has_trailing_line_comment(&self, pos: BytePos) -> bool {
        self.has_trailing_line_comment_bounded(pos, BytePos(u32::MAX))
    }

    // Bounded variant: only scans positions < bound (used for property-level scans
    // so statement-level trailing comments beyond the containing structure aren't
    // claimed by the last property, which would cause duplicate emission).
    pub(super) fn has_trailing_line_comment_bounded(&self, pos: BytePos, bound: BytePos) -> bool {
        (0u32..4).any(|offset| {
            let p = pos + BytePos(offset);
            if p >= bound { return false; }
            self.comments.get_trailing(p)
                .map(|cs| cs.iter().any(|c| c.kind == swc_core::common::comments::CommentKind::Line))
                .unwrap_or(false)
        })
    }

    pub(super) fn emit_trailing_comments(&mut self, pos: BytePos) {
        self.emit_trailing_comments_bounded(pos, BytePos(u32::MAX));
    }

    // Bounded variant: only scans positions < bound. Use when emitting trailing
    // comments for the last element of a structure (object/array/call) to avoid
    // claiming statement-level comments that sit just past the closing delimiter.
    pub(super) fn emit_trailing_comments_bounded(&mut self, pos: BytePos, bound: BytePos) {
        let actual_pos = (0u32..4).find(|&offset| {
            let p = pos + BytePos(offset);
            if p >= bound { return false; }
            self.comments.get_trailing(p).map_or(false, |cs| !cs.is_empty())
        }).map(|offset| pos + BytePos(offset));
        let Some(ap) = actual_pos else { return };
        if let Some(comments) = self.comments.get_trailing(ap) {
            let stmt_line = self.cm.lookup_char_pos(pos).line;
            for c in &comments {
                match c.kind {
                    swc_core::common::comments::CommentKind::Block => {
                        self.w(" /*");
                        self.w(&c.text);
                        self.w("*/");
                    }
                    swc_core::common::comments::CommentKind::Line => {
                        let comment_line = self.cm.lookup_char_pos(c.span.lo).line;
                        if comment_line > stmt_line {
                            self.nl();
                            self.wi();
                            self.w("//");
                            self.w(&c.text);
                        } else {
                            self.w(" //");
                            self.w(&c.text);
                        }
                    }
                }
            }
        }
    }

    // ─── module items ───────────────────────────────────────

    fn print_module_item(&mut self, item: &ModuleItem) {
        // Leading comments + indentation are emitted here (the single canonical
        // place for top-level items). `print_stmt_body`/`print_module_decl` do
        // not re-emit leading comments, avoiding duplication.
        self.emit_leading_comments(item.span().lo);
        self.wi();
        match item {
            ModuleItem::ModuleDecl(d) => self.print_module_decl(d),
            ModuleItem::Stmt(s) => self.print_stmt_body(s),
        }
        // Emit trailing comments inline (e.g. `const x = 1; /* inline */`)
        if self.buf.ends_with('\n') {
            self.buf.pop();
        }
        self.emit_trailing_comments(item.span().hi);
        self.nl();
    }

    fn print_module_decl(&mut self, decl: &ModuleDecl) {
        match decl {
            ModuleDecl::Import(d) => {
                self.w("import ");
                if d.type_only { self.w("type "); }

                let has_default = d.specifiers.iter().any(|s| matches!(s, ImportSpecifier::Default(_)));
                let named: Vec<_> = d.specifiers.iter().filter(|s| matches!(s, ImportSpecifier::Named(_))).collect();
                let has_ns = d.specifiers.iter().any(|s| matches!(s, ImportSpecifier::Namespace(_)));

                // Default import
                if has_default {
                    for s in &d.specifiers {
                        if let ImportSpecifier::Default(def) = s {
                            self.w(&*def.local.sym);
                        }
                    }
                    if !named.is_empty() || has_ns {
                        self.w(", ");
                    }
                }

                // Namespace import
                if has_ns {
                    for s in &d.specifiers {
                        if let ImportSpecifier::Namespace(ns) = s {
                            self.w("* as ");
                            self.w(&*ns.local.sym);
                        }
                    }
                    if !named.is_empty() {
                        self.w(", ");
                    }
                }

                // Named imports — try inline first, expand if > max_members or too wide
                if !named.is_empty() {
                    let checkpoint = self.buf.len();
                    self.w("{ ");
                    for (i, n) in named.iter().enumerate() {
                        if i > 0 { self.w(", "); }
                        if let ImportSpecifier::Named(named_spec) = n {
                            if named_spec.is_type_only { self.w("type "); }
                            if let Some(imported) = &named_spec.imported {
                                match imported {
                                    ModuleExportName::Ident(id) => self.w(&*id.sym),
                                    ModuleExportName::Str(s) => self.print_str_lit(s.value.as_str().unwrap()),
                                }
                                self.w(" as ");
                            }
                            self.w(&*named_spec.local.sym);
                        }
                    }
                    self.w(" }");
                    if !self.inline_fits(named.len(), self.collapse.max_imports) {
                        self.buf.truncate(checkpoint);
                        self.w("{");
                        self.nl();
                        self.indent();
                        for n in named.iter() {
                            self.wi();
                            if let ImportSpecifier::Named(named_spec) = n {
                                if named_spec.is_type_only { self.w("type "); }
                                if let Some(imported) = &named_spec.imported {
                                    match imported {
                                        ModuleExportName::Ident(id) => self.w(&*id.sym),
                                        ModuleExportName::Str(s) => self.print_str_lit(s.value.as_str().unwrap()),
                                    }
                                    self.w(" as ");
                                }
                                self.w(&*named_spec.local.sym);
                            }
                            self.w(",");
                            self.nl();
                        }
                        self.dedent();
                        self.wi();
                        self.w("}");
                    }
                }

                if has_default || has_ns || !named.is_empty() {
                    self.w(" from \"");
                } else {
                    // Side-effect import: `import "./module"` — no bindings, no `from`
                    self.w("\"");
                }
                self.w(d.src.value.as_str().unwrap());
                self.w("\";");
            }
            ModuleDecl::ExportDecl(d) => {
                self.w("export ");
                self.print_decl(&d.decl);
            }
            ModuleDecl::ExportNamed(n) => {
                self.w("export ");
                if n.type_only { self.w("type "); }
                if !n.specifiers.is_empty() { self.w("{ "); }
                else { self.w("{}"); }
                for (i, s) in n.specifiers.iter().enumerate() {
                    if i > 0 { self.w(", "); }
                    match s {
                        ExportSpecifier::Named(ns) => {
                            match &ns.orig {
                                ModuleExportName::Ident(id) => self.w(&*id.sym),
                                ModuleExportName::Str(ss) => self.w(ss.value.as_str().unwrap()),
                            }
                            if let Some(exported) = &ns.exported {
                                self.w(" as ");
                                match exported {
                                    ModuleExportName::Ident(id) => self.w(&*id.sym),
                                    ModuleExportName::Str(ss) => self.w(ss.value.as_str().unwrap()),
                                }
                            }
                        }
                        ExportSpecifier::Default(ds) => self.w(&*ds.exported.sym),
                        ExportSpecifier::Namespace(ns) => {
                            self.w("* as ");
                            match &ns.name {
                                ModuleExportName::Ident(id) => self.w(&*id.sym),
                                ModuleExportName::Str(ss) => self.w(ss.value.as_str().unwrap()),
                            }
                        }
                    }
                }
                if !n.specifiers.is_empty() { self.w(" }"); }
                if let Some(src) = &n.src {
                    self.w(" from \"");
                    self.w(src.value.as_str().unwrap());
                    self.w("\"");
                }
                self.w(";");
            }
            ModuleDecl::ExportDefaultExpr(d) => {
                self.w("export default ");
                self.print_expr(&d.expr);
                self.w(";");
            }
            ModuleDecl::ExportDefaultDecl(d) => {
                self.w("export default ");
                match &d.decl {
                    DefaultDecl::Fn(f) => {
                        if f.function.is_async { self.w("async "); }
                        if let Some(id) = &f.ident {
                            self.w("function ");
                            self.w(&*id.sym);
                        } else {
                            self.w("function");
                        }
                        self.print_fn_sig(&f.function);
                        if let Some(body) = &f.function.body {
                            self.w(" ");
                            self.print_block(body);
                        } else {
                            self.w(";");
                        }
                    }
                    DefaultDecl::Class(c) => {
                        if c.class.is_abstract { self.w("abstract "); }
                        self.w("class");
                        if let Some(id) = &c.ident { self.w(" "); self.w(&*id.sym); }
                        self.print_class(&c.class);
                    }
                    DefaultDecl::TsInterfaceDecl(i) => {
                        self.w("interface ");
                        self.w(&*i.id.sym);
                        if let Some(ext) = i.extends.first() {
                            self.w(" extends ");
                            self.print_expr(&ext.expr);
                        }
                        self.w(" {");
                        self.nl();
                        self.indent();
                        for m in &i.body.body {
                            self.wi();
                            self.print_ts_member(m);
                        }
                        self.dedent();
                        self.wi();
                        self.w("}");
                    }
                }
            }
            ModuleDecl::ExportAll(e) => {
                self.w("export * from \"");
                self.w(e.src.value.as_str().unwrap());
                self.w("\";");
            }
            _ => { self.w("// unhandled module decl"); self.nl(); }
        }
    }
}

// ─── public entry point ──────────────────────────────────────

pub(crate) fn format_js_with_printer(
    code: &str,
    indent: &str,
    wrap_width: usize,
    collapse: crate::full::format::CollapseConfig,
    _remove_unused: bool,
) -> String {
    if code.trim().is_empty() {
        return code.to_string();
    }

    let cm: Lrc<SourceMap> = Lrc::new(SourceMap::default());
    let fm = cm.new_source_file(FileName::Anon.into(), code.to_string());
    let comments = SingleThreadedComments::default();

    let module = parse_ts(&fm, &comments).or_else(|| parse_es(&fm, &comments));
    let module = match module {
        Some(m) => m,
        None => return code.to_string(),
    };

    let printer = Printer::new(indent, wrap_width, collapse, &comments, cm);
    printer.print_module(&module)
}

fn parse_ts(fm: &swc_core::common::SourceFile, comments: &SingleThreadedComments) -> Option<Module> {
    let syntax = Syntax::Typescript(TsSyntax { tsx: false, decorators: false, ..Default::default() });
    let input = StringInput::new(&fm.src, fm.start_pos, fm.end_pos);
    let lexer = Lexer::new(syntax, EsVersion::latest(), input, Some(comments));
    let mut parser = Parser::new_from(lexer);
    parser.parse_module().ok()
}

fn parse_es(fm: &swc_core::common::SourceFile, comments: &SingleThreadedComments) -> Option<Module> {
    let syntax = Syntax::Es(Default::default());
    let input = StringInput::new(&fm.src, fm.start_pos, fm.end_pos);
    let lexer = Lexer::new(syntax, EsVersion::latest(), input, Some(comments));
    let mut parser = Parser::new_from(lexer);
    parser.parse_module().ok()
}

/// Escape a decoded string value for emission inside double quotes.
fn escape_str_content(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{08}' => out.push_str("\\b"),
            '\u{0C}' => out.push_str("\\f"),
            c if (c as u32) < 0x20 => {
                out.push_str(&format!("\\u{:04x}", c as u32));
            }
            c => out.push(c),
        }
    }
    out
}
