//! AST semantic safety check — verifies formatting did not change code semantics.
//!
//! After the formatter produces output, re-parse it and compare the semantic
//! token sequences of the original and formatted ASTs. If they differ, the
//! formatter has corrupted the code — callers must not write the file.
//!
//! Also verifies that all comments from the original source are present in the
//! formatted output, for all file types.

use swc_core::ecma::ast::*;
use swc_core::common::input::StringInput;
use swc_core::common::sync::Lrc;
use swc_core::common::{FileName, SourceMap, Spanned};
use swc_core::ecma::parser::lexer::Lexer;
use swc_core::ecma::parser::{Parser, Syntax, TsSyntax};
use swc_core::ecma::ast::EsVersion;

/// Parse source and collect semantic tokens in document order.
/// Returns `Ok(tokens)` on success, `Err(diagnostic)` if the source cannot be parsed.
pub(crate) fn collect_tokens(src: &str) -> Result<Vec<String>, String> {
    let cm: Lrc<SourceMap> = Lrc::new(SourceMap::default());
    let fm = cm.new_source_file(FileName::Anon.into(), src.to_string());
    let comments = swc_core::common::comments::SingleThreadedComments::default();

    let syntax = Syntax::Typescript(TsSyntax { tsx: false, decorators: false, ..Default::default() });
    let input = StringInput::new(&fm.src, fm.start_pos, fm.end_pos);
    let lexer = Lexer::new(syntax, EsVersion::latest(), input, Some(&comments));
    let mut parser = Parser::new_from(lexer);
    let module = parser.parse_module().map_err(|e| {
        // Translate the byte offset into a line:col for a human-readable error.
        let lo = e.span().lo.0 as usize;
        let (line, col) = byte_offset_to_line_col(src, lo);
        format!("line {line}, col {col}: {e:?}")
    })?;

    let mut tokens = Vec::new();
    walk_module(&module, &mut tokens);
    Ok(tokens)
}

fn byte_offset_to_line_col(src: &str, offset: usize) -> (usize, usize) {
    let safe = offset.min(src.len());
    let before = &src[..safe];
    let line = before.matches('\n').count() + 1;
    let col = before.rfind('\n').map(|p| safe - p - 1).unwrap_or(safe) + 1;
    (line, col)
}

/// Verify that formatting did not change the semantic content of the source.
pub(crate) fn verify_semantics_preserved(original: &str, formatted: &str) -> Result<(), String> {
    let orig = match collect_tokens(original) {
        Ok(t) => t,
        Err(_) => return Ok(()), // Can't parse original — skip check
    };
    let fmt = match collect_tokens(formatted) {
        Ok(t) => t,
        Err(diag) => return Err(format!("formatted output failed to parse as TypeScript ({diag})")),
    };
    if orig == fmt {
        return Ok(());
    }
    let min_len = orig.len().min(fmt.len());
    if let Some(i) = (0..min_len).find(|&i| orig[i] != fmt[i]) {
        let start = i.saturating_sub(3);
        let orig_ctx: Vec<_> = orig[start..=(i + 3).min(orig.len() - 1)].iter().cloned().collect();
        let fmt_ctx: Vec<_> = fmt[start..=(i + 3).min(fmt.len() - 1)].iter().cloned().collect();
        Err(format!(
            "token #{} changed: {:?} → {:?}  (context: {:?} → {:?})",
            i, orig[i], fmt[i], orig_ctx, fmt_ctx
        ))
    } else {
        Err(format!(
            "token count changed: {} → {}  (last original: {:?}, last formatted: {:?})",
            orig.len(),
            fmt.len(),
            orig.last(),
            fmt.last(),
        ))
    }
}

/// Collect all comment texts from a TS/JS source using SWC's parser.
/// Returns a sorted list of trimmed comment texts, or `None` if parsing fails.
/// Internal `__REEFMT_*` placeholder comments are excluded.
pub(crate) fn collect_comment_texts_ts(src: &str) -> Option<Vec<String>> {
    let cm: Lrc<SourceMap> = Lrc::new(SourceMap::default());
    let fm = cm.new_source_file(FileName::Anon.into(), src.to_string());
    let comments = swc_core::common::comments::SingleThreadedComments::default();

    let syntax = Syntax::Typescript(TsSyntax { tsx: false, decorators: false, ..Default::default() });
    let input = StringInput::new(&fm.src, fm.start_pos, fm.end_pos);
    let lexer = Lexer::new(syntax, EsVersion::latest(), input, Some(&comments));
    let mut parser = Parser::new_from(lexer);
    if parser.parse_module().is_err() {
        return None; // unparseable — skip comment check
    }

    let mut texts: Vec<String> = Vec::new();
    let (leading, trailing) = comments.borrow_all();
    for (_, cs) in leading.iter().chain(trailing.iter()) {
        for c in cs {
            let text = c.text.trim().to_string();
            if !text.contains("__REEFMT_") {
                texts.push(text);
            }
        }
    }
    texts.sort();
    Some(texts)
}

/// Extract comment texts from any source file using text scanning.
/// Handles `//` line comments and `/* */` block comments.
/// Skips string and template literal content so embedded `//` isn't collected.
/// Internal `__REEFMT_*` placeholder comments are excluded.
pub(crate) fn collect_comment_texts_text(src: &str) -> Vec<String> {
    let bytes = src.as_bytes();
    let len = bytes.len();
    let mut i = 0;
    let mut texts: Vec<String> = Vec::new();

    while i < len {
        // Skip double-quoted strings
        if bytes[i] == b'"' {
            i += 1;
            while i < len {
                if bytes[i] == b'\\' { i += 2; }
                else if bytes[i] == b'"' { i += 1; break; }
                else { i += 1; }
            }
            continue;
        }
        // Skip single-quoted strings
        if bytes[i] == b'\'' {
            i += 1;
            while i < len {
                if bytes[i] == b'\\' { i += 2; }
                else if bytes[i] == b'\'' { i += 1; break; }
                else { i += 1; }
            }
            continue;
        }
        // Skip template literals
        if bytes[i] == b'`' {
            i += 1;
            let mut depth = 0u32;
            while i < len {
                if bytes[i] == b'\\' { i += 2; }
                else if bytes[i] == b'$' && i + 1 < len && bytes[i + 1] == b'{' { i += 2; depth += 1; }
                else if bytes[i] == b'}' && depth > 0 { i += 1; depth -= 1; }
                else if bytes[i] == b'`' && depth == 0 { i += 1; break; }
                else { i += 1; }
            }
            continue;
        }
        // Line comment `//`
        if bytes[i] == b'/' && i + 1 < len && bytes[i + 1] == b'/' {
            i += 2;
            let start = i;
            while i < len && bytes[i] != b'\n' { i += 1; }
            let text = src[start..i].trim().to_string();
            if !text.contains("__REEFMT_") {
                texts.push(text);
            }
            continue;
        }
        // Block comment `/* ... */`
        if bytes[i] == b'/' && i + 1 < len && bytes[i + 1] == b'*' {
            i += 2;
            let start = i;
            while i + 1 < len {
                if bytes[i] == b'*' && bytes[i + 1] == b'/' { i += 2; break; }
                i += 1;
            }
            let text = src[start..i.saturating_sub(2)].trim().to_string();
            if !text.contains("__REEFMT_") {
                texts.push(text);
            }
            continue;
        }
        i += 1;
    }

    texts.sort();
    texts
}

/// Verify that every comment in `original` is present in `formatted`.
/// Uses SWC for TS/JS files (accurate, handles strings), text scanning for others.
/// Falls back gracefully if parsing fails.
pub(crate) fn verify_comments_preserved(original: &str, formatted: &str, ext: &str) -> Result<(), String> {
    let orig_texts: Vec<String>;
    let fmt_texts: Vec<String>;

    match ext {
        "ts" | "js" => {
            orig_texts = match collect_comment_texts_ts(original) {
                Some(t) => t,
                None => return Ok(()), // unparseable — skip
            };
            fmt_texts = match collect_comment_texts_ts(formatted) {
                Some(t) => t,
                None => return Ok(()),
            };
        }
        _ => {
            orig_texts = collect_comment_texts_text(original);
            fmt_texts = collect_comment_texts_text(formatted);
        }
    }

    // Every comment from original must appear in formatted (order-independent,
    // duplicate-aware: two identical comments in original need two in formatted).
    let mut remaining = fmt_texts.clone();
    for comment in &orig_texts {
        if let Some(pos) = remaining.iter().position(|c| c == comment) {
            remaining.remove(pos);
        } else {
            return Err(format!("comment lost during formatting: {:?}", comment));
        }
    }
    Ok(())
}

// ─── atom helpers ───────────────────────────────────────────────────────────

fn atom_str(a: &swc_core::atoms::Atom) -> String {
    a.as_str().to_string()
}

fn wtf8_str(a: &swc_core::atoms::Wtf8Atom) -> String {
    a.as_str().map_or_else(String::new, |s| s.to_string())
}

// ─── AST walkers ────────────────────────────────────────────────────────────

fn walk_module(m: &Module, t: &mut Vec<String>) {
    for item in &m.body { walk_module_item(item, t); }
}

fn walk_module_item(item: &ModuleItem, t: &mut Vec<String>) {
    match item {
        ModuleItem::ModuleDecl(d) => walk_module_decl(d, t),
        ModuleItem::Stmt(s) => walk_stmt(s, t),
    }
}

fn walk_module_decl(decl: &ModuleDecl, t: &mut Vec<String>) {
    match decl {
        ModuleDecl::Import(d) => {
            t.push("import".into());
            if d.type_only { t.push("type".into()); }
            for s in &d.specifiers {
                match s {
                    ImportSpecifier::Default(def) => t.push(atom_str(&def.local.sym)),
                    ImportSpecifier::Named(ns) => {
                        if ns.is_type_only { t.push("type".into()); }
                        if let Some(imported) = &ns.imported {
                            match imported {
                                ModuleExportName::Ident(id) => t.push(atom_str(&id.sym)),
                                ModuleExportName::Str(s) => t.push(wtf8_str(&s.value)),
                            }
                            t.push("as".into());
                        }
                        t.push(atom_str(&ns.local.sym));
                    }
                    ImportSpecifier::Namespace(ns) => {
                        t.push("*".into());
                        t.push("as".into());
                        t.push(atom_str(&ns.local.sym));
                    }
                }
            }
            t.push("from".into());
            t.push(wtf8_str(&d.src.value));
        }
        ModuleDecl::ExportDecl(d) => { t.push("export".into()); walk_decl(&d.decl, t); }
        ModuleDecl::ExportNamed(n) => {
            t.push("export".into());
            if n.type_only { t.push("type".into()); }
            for s in &n.specifiers {
                match s {
                    ExportSpecifier::Named(ns) => {
                        match &ns.orig {
                            ModuleExportName::Ident(id) => t.push(atom_str(&id.sym)),
                            ModuleExportName::Str(s) => t.push(wtf8_str(&s.value)),
                        }
                        if let Some(exp) = &ns.exported {
                            t.push("as".into());
                            match exp {
                                ModuleExportName::Ident(id) => t.push(atom_str(&id.sym)),
                                ModuleExportName::Str(s) => t.push(wtf8_str(&s.value)),
                            }
                        }
                    }
                    ExportSpecifier::Default(ds) => t.push(atom_str(&ds.exported.sym)),
                    ExportSpecifier::Namespace(ns) => {
                        t.push("*".into());
                        match &ns.name {
                            ModuleExportName::Ident(id) => t.push(atom_str(&id.sym)),
                            ModuleExportName::Str(s) => t.push(wtf8_str(&s.value)),
                        }
                    }
                }
            }
            if let Some(src) = &n.src {
                t.push("from".into());
                t.push(wtf8_str(&src.value));
            }
        }
        ModuleDecl::ExportDefaultExpr(d) => {
            t.push("export".into()); t.push("default".into());
            walk_expr(&d.expr, t);
        }
        ModuleDecl::ExportDefaultDecl(d) => {
            t.push("export".into()); t.push("default".into());
            match &d.decl {
                DefaultDecl::Fn(f) => {
                    t.push("function".into());
                    if let Some(id) = &f.ident { t.push(atom_str(&id.sym)); }
                    walk_function(&f.function, t);
                }
                DefaultDecl::Class(c) => {
                    t.push("class".into());
                    if let Some(id) = &c.ident { t.push(atom_str(&id.sym)); }
                    walk_class(&c.class, t);
                }
                _ => {}
            }
        }
        ModuleDecl::ExportAll(e) => {
            t.push("export".into()); t.push("*".into());
            t.push("from".into()); t.push(wtf8_str(&e.src.value));
        }
        _ => {}
    }
}

fn walk_stmt(stmt: &Stmt, t: &mut Vec<String>) {
    match stmt {
        Stmt::Block(b) => { for s in &b.stmts { walk_stmt(s, t); } }
        Stmt::Empty(_) => {}
        Stmt::Debugger(_) => t.push("debugger".into()),
        Stmt::Return(r) => {
            t.push("return".into());
            if let Some(arg) = &r.arg { walk_expr(arg, t); }
        }
        Stmt::If(i) => {
            t.push("if".into());
            walk_expr(&i.test, t);
            walk_stmt(&i.cons, t);
            if let Some(alt) = &i.alt { walk_stmt(alt, t); }
        }
        Stmt::While(w) => { t.push("while".into()); walk_expr(&w.test, t); walk_stmt(&w.body, t); }
        Stmt::DoWhile(d) => { t.push("do".into()); walk_stmt(&d.body, t); t.push("while".into()); walk_expr(&d.test, t); }
        Stmt::For(f) => {
            t.push("for".into());
            if let Some(init) = &f.init {
                match init {
                    VarDeclOrExpr::VarDecl(v) => walk_var_decl(v, t),
                    VarDeclOrExpr::Expr(e) => walk_expr(e, t),
                }
            }
            if let Some(test) = &f.test { walk_expr(test, t); }
            if let Some(update) = &f.update { walk_expr(update, t); }
            walk_stmt(&f.body, t);
        }
        Stmt::ForIn(fi) => {
            t.push("for".into()); t.push("in".into());
            walk_for_head(&fi.left, t); walk_expr(&fi.right, t); walk_stmt(&fi.body, t);
        }
        Stmt::ForOf(fo) => {
            t.push("for".into()); t.push("of".into());
            walk_for_head(&fo.left, t); walk_expr(&fo.right, t); walk_stmt(&fo.body, t);
        }
        Stmt::Try(tr) => {
            t.push("try".into());
            for s in &tr.block.stmts { walk_stmt(s, t); }
            if let Some(catch) = &tr.handler {
                t.push("catch".into());
                if let Some(param) = &catch.param { walk_pat(param, t); }
                for s in &catch.body.stmts { walk_stmt(s, t); }
            }
            if let Some(fin) = &tr.finalizer {
                t.push("finally".into());
                for s in &fin.stmts { walk_stmt(s, t); }
            }
        }
        Stmt::Switch(s) => {
            t.push("switch".into());
            walk_expr(&s.discriminant, t);
            for case in &s.cases {
                if let Some(test) = &case.test { walk_expr(test, t); }
                for s in &case.cons { walk_stmt(s, t); }
            }
        }
        Stmt::Throw(th) => { t.push("throw".into()); walk_expr(&th.arg, t); }
        Stmt::Decl(d) => walk_decl(d, t),
        Stmt::Expr(e) => walk_expr(&e.expr, t),
        Stmt::Break(b) => {
            t.push("break".into());
            if let Some(label) = &b.label { t.push(atom_str(&label.sym)); }
        }
        Stmt::Continue(c) => {
            t.push("continue".into());
            if let Some(label) = &c.label { t.push(atom_str(&label.sym)); }
        }
        Stmt::Labeled(l) => { t.push(atom_str(&l.label.sym)); walk_stmt(&l.body, t); }
        _ => {}
    }
}

fn walk_decl(decl: &Decl, t: &mut Vec<String>) {
    match decl {
        Decl::Fn(f) => {
            t.push("function".into());
            t.push(atom_str(&f.ident.sym));
            walk_function(&f.function, t);
        }
        Decl::Var(v) => walk_var_decl(v, t),
        Decl::Class(c) => {
            t.push("class".into());
            t.push(atom_str(&c.ident.sym));
            walk_class(&c.class, t);
        }
        Decl::TsInterface(i) => {
            t.push("interface".into());
            t.push(atom_str(&i.id.sym));
            for m in &i.body.body { walk_ts_type_element(m, t); }
        }
        Decl::TsTypeAlias(a) => {
            t.push("type".into());
            t.push(atom_str(&a.id.sym));
            walk_ts_type(&a.type_ann, t);
        }
        Decl::TsEnum(e) => {
            t.push("enum".into());
            t.push(atom_str(&e.id.sym));
            for m in &e.members {
                match &m.id {
                    TsEnumMemberId::Ident(id) => t.push(atom_str(&id.sym)),
                    TsEnumMemberId::Str(s) => t.push(wtf8_str(&s.value)),
                }
                if let Some(init) = &m.init { walk_expr(init, t); }
            }
        }
        _ => {}
    }
}

fn walk_var_decl(v: &VarDecl, t: &mut Vec<String>) {
    let kind = match v.kind { VarDeclKind::Var => "var", VarDeclKind::Let => "let", VarDeclKind::Const => "const" };
    t.push(kind.into());
    for d in &v.decls {
        walk_pat(&d.name, t);
        if let Some(init) = &d.init { walk_expr(init, t); }
    }
}

fn walk_for_head(left: &ForHead, t: &mut Vec<String>) {
    match left {
        ForHead::VarDecl(v) => walk_var_decl(v, t),
        ForHead::Pat(p) => walk_pat(p, t),
        ForHead::UsingDecl(_) => {}
    }
}

fn walk_function(f: &Function, t: &mut Vec<String>) {
    for p in &f.params { walk_pat(&p.pat, t); }
    if let Some(ty) = &f.return_type { walk_ts_type(&ty.type_ann, t); }
    if let Some(body) = &f.body { for s in &body.stmts { walk_stmt(s, t); } }
}

fn walk_class(cls: &Class, t: &mut Vec<String>) {
    if let Some(sup) = &cls.super_class { walk_expr(sup, t); }
    for member in &cls.body {
        match member {
            ClassMember::Constructor(c) => {
                t.push("constructor".into());
                for p in &c.params {
                    match p {
                        ParamOrTsParamProp::Param(p) => walk_pat(&p.pat, t),
                        ParamOrTsParamProp::TsParamProp(p) => {
                            match &p.param {
                                TsParamPropParam::Ident(i) => t.push(atom_str(&i.id.sym)),
                                TsParamPropParam::Assign(a) => { walk_pat(&a.left, t); walk_expr(&a.right, t); }
                            }
                        }
                    }
                }
                if let Some(body) = &c.body { for s in &body.stmts { walk_stmt(s, t); } }
            }
            ClassMember::Method(m) => { walk_prop_name(&m.key, t); walk_function(&m.function, t); }
            ClassMember::PrivateMethod(m) => { t.push(format!("#{}", m.key.name)); walk_function(&m.function, t); }
            ClassMember::ClassProp(p) => {
                walk_prop_name(&p.key, t);
                if let Some(ty) = &p.type_ann { walk_ts_type(&ty.type_ann, t); }
                if let Some(val) = &p.value { walk_expr(val, t); }
            }
            ClassMember::PrivateProp(p) => {
                t.push(format!("#{}", p.key.name));
                if let Some(val) = &p.value { walk_expr(val, t); }
            }
            ClassMember::StaticBlock(b) => { for s in &b.body.stmts { walk_stmt(s, t); } }
            _ => {}
        }
    }
}

fn walk_expr(expr: &Expr, t: &mut Vec<String>) {
    match expr {
        Expr::This(_) => t.push("this".into()),
        Expr::Ident(id) => t.push(atom_str(&id.sym)),
        Expr::Lit(l) => walk_lit(l, t),
        Expr::Array(a) => {
            for e in &a.elems { if let Some(e) = e { walk_expr(&e.expr, t); } }
        }
        Expr::Object(o) => {
            for p in &o.props {
                match p {
                    PropOrSpread::Spread(s) => { t.push("...".into()); walk_expr(&s.expr, t); }
                    PropOrSpread::Prop(p) => walk_prop(p, t),
                }
            }
        }
        Expr::Fn(f) => {
            if let Some(id) = &f.ident { t.push(atom_str(&id.sym)); }
            walk_function(&f.function, t);
        }
        Expr::Arrow(a) => {
            for p in &a.params { walk_pat(p, t); }
            if let Some(ret) = &a.return_type { walk_ts_type(&ret.type_ann, t); }
            match a.body.as_ref() {
                BlockStmtOrExpr::BlockStmt(b) => { for s in &b.stmts { walk_stmt(s, t); } }
                BlockStmtOrExpr::Expr(e) => walk_expr(e, t),
            }
        }
        Expr::Unary(u) => {
            let op = match u.op {
                UnaryOp::Minus => "-", UnaryOp::Plus => "+", UnaryOp::Bang => "!",
                UnaryOp::Tilde => "~", UnaryOp::TypeOf => "typeof", UnaryOp::Void => "void",
                UnaryOp::Delete => "delete",
            };
            t.push(op.into());
            walk_expr(&u.arg, t);
        }
        Expr::Update(u) => {
            let op = if u.op == UpdateOp::PlusPlus { "++" } else { "--" };
            t.push(op.into());
            walk_expr(&u.arg, t);
        }
        Expr::Bin(b) => {
            walk_expr(&b.left, t);
            let op = match b.op {
                BinaryOp::EqEq => "==", BinaryOp::NotEq => "!=",
                BinaryOp::EqEqEq => "===", BinaryOp::NotEqEq => "!==",
                BinaryOp::Lt => "<", BinaryOp::LtEq => "<=",
                BinaryOp::Gt => ">", BinaryOp::GtEq => ">=",
                BinaryOp::Add => "+", BinaryOp::Sub => "-",
                BinaryOp::Mul => "*", BinaryOp::Div => "/", BinaryOp::Mod => "%",
                BinaryOp::BitAnd => "&", BinaryOp::BitOr => "|", BinaryOp::BitXor => "^",
                BinaryOp::LogicalAnd => "&&", BinaryOp::LogicalOr => "||",
                BinaryOp::NullishCoalescing => "??", BinaryOp::In => "in",
                BinaryOp::InstanceOf => "instanceof", BinaryOp::Exp => "**",
                BinaryOp::LShift => "<<", BinaryOp::RShift => ">>",
                BinaryOp::ZeroFillRShift => ">>>",
            };
            t.push(op.into());
            walk_expr(&b.right, t);
        }
        Expr::Assign(a) => {
            walk_assign_target(&a.left, t);
            let op = match a.op {
                AssignOp::Assign => "=", AssignOp::AddAssign => "+=",
                AssignOp::SubAssign => "-=", AssignOp::MulAssign => "*=",
                AssignOp::DivAssign => "/=", AssignOp::ModAssign => "%=",
                AssignOp::LShiftAssign => "<<=", AssignOp::RShiftAssign => ">>=",
                AssignOp::ZeroFillRShiftAssign => ">>>=",
                AssignOp::BitOrAssign => "|=", AssignOp::BitXorAssign => "^=",
                AssignOp::BitAndAssign => "&=", AssignOp::ExpAssign => "**=",
                AssignOp::AndAssign => "&&=", AssignOp::OrAssign => "||=",
                AssignOp::NullishAssign => "??=",
            };
            t.push(op.into());
            walk_expr(&a.right, t);
        }
        Expr::Member(m) => {
            walk_expr(&m.obj, t);
            match &m.prop {
                MemberProp::Ident(id) => t.push(atom_str(&id.sym)),
                MemberProp::PrivateName(p) => t.push(format!("#{}", p.name)),
                MemberProp::Computed(c) => walk_expr(&c.expr, t),
            }
        }
        Expr::SuperProp(sp) => {
            t.push("super".into());
            match &sp.prop {
                SuperProp::Ident(id) => t.push(atom_str(&id.sym)),
                SuperProp::Computed(c) => walk_expr(&c.expr, t),
            }
        }
        Expr::Cond(c) => { walk_expr(&c.test, t); walk_expr(&c.cons, t); walk_expr(&c.alt, t); }
        Expr::Call(c) => {
            walk_callee(&c.callee, t);
            for a in &c.args { if a.spread.is_some() { t.push("...".into()); } walk_expr(&a.expr, t); }
        }
        Expr::New(n) => {
            t.push("new".into());
            walk_expr(&n.callee, t);
            if let Some(args) = &n.args {
                for a in args { if a.spread.is_some() { t.push("...".into()); } walk_expr(&a.expr, t); }
            }
        }
        Expr::Seq(s) => { for e in &s.exprs { walk_expr(e, t); } }
        Expr::Tpl(tpl) => {
            for q in &tpl.quasis { t.push(q.raw.to_string()); }
            for e in &tpl.exprs { walk_expr(e, t); }
        }
        Expr::TaggedTpl(tpl) => {
            walk_expr(&tpl.tag, t);
            for q in &tpl.tpl.quasis { t.push(q.raw.to_string()); }
            for e in &tpl.tpl.exprs { walk_expr(e, t); }
        }
        Expr::Await(a) => { t.push("await".into()); walk_expr(&a.arg, t); }
        Expr::Yield(y) => { t.push("yield".into()); if let Some(arg) = &y.arg { walk_expr(arg, t); } }
        Expr::Paren(p) => walk_expr(&p.expr, t),
        Expr::TsAs(a) => { walk_expr(&a.expr, t); walk_ts_type(&a.type_ann, t); }
        Expr::TsNonNull(n) => walk_expr(&n.expr, t),
        Expr::TsTypeAssertion(a) => { walk_ts_type(&a.type_ann, t); walk_expr(&a.expr, t); }
        Expr::TsConstAssertion(a) => walk_expr(&a.expr, t),
        Expr::TsSatisfies(s) => { walk_expr(&s.expr, t); walk_ts_type(&s.type_ann, t); }
        Expr::TsInstantiation(i) => {
            walk_expr(&i.expr, t);
            for a in &i.type_args.params { walk_ts_type(a, t); }
        }
        Expr::OptChain(o) => {
            match o.base.as_ref() {
                OptChainBase::Member(m) => {
                    walk_expr(&m.obj, t);
                    match &m.prop {
                        MemberProp::Ident(id) => t.push(atom_str(&id.sym)),
                        MemberProp::PrivateName(p) => t.push(format!("#{}", p.name)),
                        MemberProp::Computed(c) => walk_expr(&c.expr, t),
                    }
                }
                OptChainBase::Call(c) => {
                    walk_expr(&c.callee, t);
                    for a in &c.args { if a.spread.is_some() { t.push("...".into()); } walk_expr(&a.expr, t); }
                }
            }
        }
        Expr::MetaProp(m) => {
            t.push(match m.kind { MetaPropKind::NewTarget => "new.target", MetaPropKind::ImportMeta => "import.meta" }.into());
        }
        Expr::PrivateName(p) => t.push(format!("#{}", p.name)),
        Expr::Class(c) => {
            if let Some(id) = &c.ident { t.push(atom_str(&id.sym)); }
            walk_class(&c.class, t);
        }
        _ => {}
    }
}

fn walk_lit(lit: &Lit, t: &mut Vec<String>) {
    match lit {
        Lit::Str(s) => t.push(format!("\"{}\"", wtf8_str(&s.value))),
        Lit::Num(n) => t.push(n.raw.as_deref().unwrap_or("").to_string()),
        Lit::Bool(b) => t.push(if b.value { "true" } else { "false" }.into()),
        Lit::Null(_) => t.push("null".into()),
        Lit::BigInt(b) => t.push(format!("{}n", b.value)),
        Lit::Regex(r) => t.push(format!("/{}/{}", r.exp, r.flags)),
        _ => {}
    }
}

fn walk_prop(prop: &Prop, t: &mut Vec<String>) {
    match prop {
        Prop::Shorthand(id) => t.push(atom_str(&id.sym)),
        Prop::KeyValue(kv) => { walk_prop_name(&kv.key, t); walk_expr(&kv.value, t); }
        Prop::Assign(a) => { t.push(atom_str(&a.key.sym)); walk_expr(&a.value, t); }
        Prop::Getter(g) => {
            t.push("get".into());
            walk_prop_name(&g.key, t);
            if let Some(body) = &g.body { for s in &body.stmts { walk_stmt(s, t); } }
        }
        Prop::Setter(s) => {
            t.push("set".into());
            walk_prop_name(&s.key, t);
            if let Some(body) = &s.body { for s in &body.stmts { walk_stmt(s, t); } }
        }
        Prop::Method(m) => { walk_prop_name(&m.key, t); walk_function(&m.function, t); }
    }
}

fn walk_prop_name(name: &PropName, t: &mut Vec<String>) {
    match name {
        PropName::Ident(id) => t.push(atom_str(&id.sym)),
        PropName::Str(s) => t.push(format!("\"{}\"", wtf8_str(&s.value))),
        PropName::Num(n) => t.push(n.value.to_string()),
        PropName::Computed(c) => walk_expr(&c.expr, t),
        PropName::BigInt(b) => t.push(format!("{}n", b.value)),
    }
}

fn walk_pat(pat: &Pat, t: &mut Vec<String>) {
    match pat {
        Pat::Ident(id) => {
            t.push(atom_str(&id.id.sym));
            if let Some(ty) = &id.type_ann { walk_ts_type(&ty.type_ann, t); }
        }
        Pat::Array(a) => {
            for e in &a.elems { if let Some(e) = e { walk_pat(e, t); } }
            if let Some(ty) = &a.type_ann { walk_ts_type(&ty.type_ann, t); }
        }
        Pat::Rest(r) => { t.push("...".into()); walk_pat(&r.arg, t); }
        Pat::Object(o) => {
            for p in &o.props { walk_object_pat_prop(p, t); }
            if let Some(ty) = &o.type_ann { walk_ts_type(&ty.type_ann, t); }
        }
        Pat::Assign(a) => { walk_pat(&a.left, t); walk_expr(&a.right, t); }
        Pat::Expr(e) => walk_expr(e, t),
        Pat::Invalid(_) => {}
    }
}

fn walk_object_pat_prop(prop: &ObjectPatProp, t: &mut Vec<String>) {
    match prop {
        ObjectPatProp::KeyValue(kv) => { walk_prop_name(&kv.key, t); walk_pat(&kv.value, t); }
        ObjectPatProp::Assign(a) => { t.push(atom_str(&a.key.sym)); if let Some(v) = &a.value { walk_expr(v, t); } }
        ObjectPatProp::Rest(r) => { t.push("...".into()); walk_pat(&r.arg, t); }
    }
}

fn walk_assign_target(target: &AssignTarget, t: &mut Vec<String>) {
    match target {
        AssignTarget::Simple(s) => walk_simple_assign_target(s, t),
        AssignTarget::Pat(p) => match p {
            AssignTargetPat::Array(a) => { for e in &a.elems { if let Some(e) = e { walk_pat(e, t); } } }
            AssignTargetPat::Object(o) => { for p in &o.props { walk_object_pat_prop(p, t); } }
            AssignTargetPat::Invalid(_) => {}
        }
    }
}

fn walk_simple_assign_target(target: &SimpleAssignTarget, t: &mut Vec<String>) {
    match target {
        SimpleAssignTarget::Ident(i) => t.push(atom_str(&i.id.sym)),
        SimpleAssignTarget::Member(m) => {
            walk_expr(&m.obj, t);
            match &m.prop {
                MemberProp::Ident(id) => t.push(atom_str(&id.sym)),
                MemberProp::PrivateName(p) => t.push(format!("#{}", p.name)),
                MemberProp::Computed(c) => walk_expr(&c.expr, t),
            }
        }
        SimpleAssignTarget::SuperProp(sp) => {
            t.push("super".into());
            match &sp.prop {
                SuperProp::Ident(id) => t.push(atom_str(&id.sym)),
                SuperProp::Computed(c) => walk_expr(&c.expr, t),
            }
        }
        SimpleAssignTarget::Paren(p) => walk_expr(&p.expr, t),
        SimpleAssignTarget::OptChain(o) => {
            match o.base.as_ref() {
                OptChainBase::Member(m) => {
                    walk_expr(&m.obj, t);
                    match &m.prop {
                        MemberProp::Ident(id) => t.push(atom_str(&id.sym)),
                        MemberProp::PrivateName(p) => t.push(format!("#{}", p.name)),
                        MemberProp::Computed(c) => walk_expr(&c.expr, t),
                    }
                }
                OptChainBase::Call(c) => {
                    walk_expr(&c.callee, t);
                    for a in &c.args { walk_expr(&a.expr, t); }
                }
            }
        }
        SimpleAssignTarget::TsAs(a) => walk_expr(&a.expr, t),
        SimpleAssignTarget::TsSatisfies(s) => walk_expr(&s.expr, t),
        SimpleAssignTarget::TsNonNull(n) => walk_expr(&n.expr, t),
        SimpleAssignTarget::TsTypeAssertion(a) => walk_expr(&a.expr, t),
        SimpleAssignTarget::TsInstantiation(i) => walk_expr(&i.expr, t),
        SimpleAssignTarget::Invalid(_) => {}
    }
}

fn walk_callee(callee: &Callee, t: &mut Vec<String>) {
    match callee {
        Callee::Super(_) => t.push("super".into()),
        Callee::Expr(e) => walk_expr(e, t),
        Callee::Import(_) => t.push("import".into()),
    }
}

fn walk_ts_type(ty: &TsType, t: &mut Vec<String>) {
    match ty {
        TsType::TsKeywordType(k) => {
            let kw = match k.kind {
                TsKeywordTypeKind::TsAnyKeyword => "any",
                TsKeywordTypeKind::TsNumberKeyword => "number",
                TsKeywordTypeKind::TsStringKeyword => "string",
                TsKeywordTypeKind::TsBooleanKeyword => "boolean",
                TsKeywordTypeKind::TsNullKeyword => "null",
                TsKeywordTypeKind::TsUndefinedKeyword => "undefined",
                TsKeywordTypeKind::TsVoidKeyword => "void",
                TsKeywordTypeKind::TsNeverKeyword => "never",
                TsKeywordTypeKind::TsUnknownKeyword => "unknown",
                TsKeywordTypeKind::TsSymbolKeyword => "symbol",
                TsKeywordTypeKind::TsBigIntKeyword => "bigint",
                TsKeywordTypeKind::TsObjectKeyword => "object",
                TsKeywordTypeKind::TsIntrinsicKeyword => "intrinsic",
            };
            t.push(kw.into());
        }
        TsType::TsTypeRef(r) => {
            walk_ts_entity_name(&r.type_name, t);
            if let Some(params) = &r.type_params {
                for p in &params.params { walk_ts_type(p, t); }
            }
        }
        TsType::TsTypeQuery(q) => {
            t.push("typeof".into());
            match &q.expr_name {
                TsTypeQueryExpr::TsEntityName(name) => walk_ts_entity_name(name, t),
                TsTypeQueryExpr::Import(i) => t.push(wtf8_str(&i.arg.value)),
            }
            if let Some(args) = &q.type_args {
                for a in &args.params { walk_ts_type(a, t); }
            }
        }
        TsType::TsArrayType(a) => walk_ts_type(&a.elem_type, t),
        TsType::TsUnionOrIntersectionType(u) => match u {
            TsUnionOrIntersectionType::TsUnionType(u) => { for ty in &u.types { walk_ts_type(ty, t); } }
            TsUnionOrIntersectionType::TsIntersectionType(i) => { for ty in &i.types { walk_ts_type(ty, t); } }
        },
        TsType::TsConditionalType(c) => {
            walk_ts_type(&c.check_type, t); walk_ts_type(&c.extends_type, t);
            walk_ts_type(&c.true_type, t); walk_ts_type(&c.false_type, t);
        }
        TsType::TsFnOrConstructorType(f) => match f {
            TsFnOrConstructorType::TsFnType(ft) => {
                for p in &ft.params { walk_ts_fn_param(p, t); }
                walk_ts_type(&ft.type_ann.type_ann, t);
            }
            TsFnOrConstructorType::TsConstructorType(ct) => {
                for p in &ct.params { walk_ts_fn_param(p, t); }
                walk_ts_type(&ct.type_ann.type_ann, t);
            }
        },
        TsType::TsTypeLit(l) => { for m in &l.members { walk_ts_type_element(m, t); } }
        TsType::TsTupleType(tup) => { for e in &tup.elem_types { walk_ts_type(&e.ty, t); } }
        TsType::TsInferType(i) => t.push(atom_str(&i.type_param.name.sym)),
        TsType::TsParenthesizedType(p) => walk_ts_type(&p.type_ann, t),
        TsType::TsTypeOperator(o) => walk_ts_type(&o.type_ann, t),
        TsType::TsIndexedAccessType(i) => { walk_ts_type(&i.obj_type, t); walk_ts_type(&i.index_type, t); }
        TsType::TsMappedType(m) => {
            t.push(atom_str(&m.type_param.name.sym));
            if let Some(constraint) = &m.type_param.constraint { walk_ts_type(constraint, t); }
            if let Some(ty) = &m.type_ann { walk_ts_type(ty, t); }
        }
        TsType::TsLitType(l) => match &l.lit {
            TsLit::Str(s) => t.push(wtf8_str(&s.value)),
            TsLit::Number(n) => t.push(n.value.to_string()),
            TsLit::Bool(b) => t.push(if b.value { "true" } else { "false" }.into()),
            _ => {}
        },
        TsType::TsThisType(_) => t.push("this".into()),
        TsType::TsOptionalType(o) => walk_ts_type(&o.type_ann, t),
        TsType::TsRestType(r) => walk_ts_type(&r.type_ann, t),
        _ => {}
    }
}

fn walk_ts_entity_name(name: &TsEntityName, t: &mut Vec<String>) {
    match name {
        TsEntityName::Ident(id) => t.push(atom_str(&id.sym)),
        TsEntityName::TsQualifiedName(q) => { walk_ts_entity_name(&q.left, t); t.push(atom_str(&q.right.sym)); }
    }
}

fn walk_ts_type_element(m: &TsTypeElement, t: &mut Vec<String>) {
    match m {
        TsTypeElement::TsPropertySignature(p) => {
            walk_expr(&p.key, t);
            if let Some(ty) = &p.type_ann { walk_ts_type(&ty.type_ann, t); }
        }
        TsTypeElement::TsMethodSignature(m) => {
            walk_expr(&m.key, t);
            for p in &m.params { walk_ts_fn_param(p, t); }
            if let Some(ret) = &m.type_ann { walk_ts_type(&ret.type_ann, t); }
        }
        TsTypeElement::TsCallSignatureDecl(c) => {
            for p in &c.params { walk_ts_fn_param(p, t); }
            if let Some(ret) = &c.type_ann { walk_ts_type(&ret.type_ann, t); }
        }
        TsTypeElement::TsConstructSignatureDecl(c) => {
            for p in &c.params { walk_ts_fn_param(p, t); }
            if let Some(ret) = &c.type_ann { walk_ts_type(&ret.type_ann, t); }
        }
        TsTypeElement::TsIndexSignature(s) => {
            for p in &s.params { walk_ts_fn_param(p, t); }
            if let Some(ty) = &s.type_ann { walk_ts_type(&ty.type_ann, t); }
        }
        _ => {}
    }
}

fn walk_ts_fn_param(param: &TsFnParam, t: &mut Vec<String>) {
    match param {
        TsFnParam::Ident(i) => {
            t.push(atom_str(&i.id.sym));
            if let Some(ty) = &i.type_ann { walk_ts_type(&ty.type_ann, t); }
        }
        TsFnParam::Rest(r) => { t.push("...".into()); walk_pat(&r.arg, t); }
        TsFnParam::Array(a) => { for e in &a.elems { if let Some(e) = e { walk_pat(e, t); } } }
        TsFnParam::Object(o) => { for p in &o.props { walk_object_pat_prop(p, t); } }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verify_accepts_identical_tokens() {
        let src = "const x = 1;\n";
        assert!(verify_semantics_preserved(src, src).is_ok());
    }

    #[test]
    fn verify_accepts_whitespace_only_change() {
        let original  = "const x   =   1;\n";
        let formatted = "const x = 1;\n";
        assert!(verify_semantics_preserved(original, formatted).is_ok());
    }

    #[test]
    fn verify_detects_changed_identifier() {
        let original  = "const x = 1;\n";
        let corrupted = "const y = 1;\n";
        assert!(verify_semantics_preserved(original, corrupted).is_err());
    }

    #[test]
    fn verify_detects_dropped_token() {
        let original  = "const x = a + b;\n";
        let corrupted = "const x = a;\n";
        assert!(verify_semantics_preserved(original, corrupted).is_err());
    }

    #[test]
    fn verify_detects_added_token() {
        let original  = "const x = a;\n";
        let corrupted = "const x = a + b;\n";
        assert!(verify_semantics_preserved(original, corrupted).is_err());
    }

    // ─── comment preservation tests ──────────────────────────────────────────

    #[test]
    fn comments_preserved_when_unchanged() {
        let src = "const x = 1; // my comment\n";
        assert!(verify_comments_preserved(src, src, "ts").is_ok());
    }

    #[test]
    fn comments_preserved_whitespace_reformatted() {
        let orig = "const x = 1; // my comment\n";
        let fmt  = "const x = 1; // my comment\n";
        assert!(verify_comments_preserved(orig, fmt, "ts").is_ok());
    }

    #[test]
    fn comments_detects_dropped_line_comment_ts() {
        let orig = "const x = 1; // important\n";
        let fmt  = "const x = 1;\n";
        assert!(verify_comments_preserved(orig, fmt, "ts").is_err());
    }

    #[test]
    fn comments_detects_dropped_block_comment_ts() {
        let orig = "/* header */\nconst x = 1;\n";
        let fmt  = "const x = 1;\n";
        assert!(verify_comments_preserved(orig, fmt, "ts").is_err());
    }

    #[test]
    fn comments_text_scanner_line_comment() {
        let texts = collect_comment_texts_text("const x = 1; // hello\n");
        assert_eq!(texts, vec!["hello"]);
    }

    #[test]
    fn comments_text_scanner_block_comment() {
        let texts = collect_comment_texts_text("/* block */\nconst x = 1;\n");
        assert_eq!(texts, vec!["block"]);
    }

    #[test]
    fn comments_text_scanner_skips_string_content() {
        // `//` inside a string must not be treated as a comment
        let texts = collect_comment_texts_text("const s = \"http://example.com\";\n");
        assert!(texts.is_empty(), "got: {:?}", texts);
    }

    #[test]
    fn comments_text_scanner_detects_dropped_css_comment() {
        let orig = "/* color: red */\n.foo { color: red; }\n";
        let fmt  = ".foo { color: red; }\n";
        assert!(verify_comments_preserved(orig, fmt, "css").is_err());
    }

    #[test]
    fn comments_duplicate_preserved() {
        // Two identical comments in original → both must appear in formatted
        let orig = "// note\nconst a = 1; // note\n";
        let fmt_ok  = "// note\nconst a = 1; // note\n";
        let fmt_bad = "const a = 1; // note\n"; // one dropped
        assert!(verify_comments_preserved(orig, fmt_ok, "ts").is_ok());
        assert!(verify_comments_preserved(orig, fmt_bad, "ts").is_err());
    }
}
