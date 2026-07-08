use swc_core::ecma::ast::*;
use swc_core::common::{Spanned, BytePos};
use swc_core::common::comments::Comments;
use super::Printer;

impl<'a> Printer<'a> {
    pub(super) fn print_expr(&mut self, expr: &Expr) {
        match expr {
            Expr::This(_) => self.w("this"),
            Expr::Ident(id) => self.w(&*id.sym),
            Expr::Lit(l) => self.print_lit(l),
            Expr::Tpl(t) => self.print_tpl(t),
            Expr::Array(a) => 'arr: {
                // Array literals: try inline, expand if too many members or too wide
                if self.collapse.enabled {
                    let checkpoint = self.buf.len();
                    self.w("[");
                    for (i, e) in a.elems.iter().enumerate() {
                        if i > 0 { self.w(", "); }
                        if let Some(e) = e {
                            self.emit_leading_comments(e.span().lo());
                            if e.spread.is_some() { self.w("..."); }
                            self.print_expr(&e.expr);
                            self.emit_trailing_comments_bounded(e.expr.span().hi(), a.span.hi());
                        }
                    }
                    // Emit closing-bracket comments so they force expansion when present
                    self.emit_leading_comments(a.span.hi() - BytePos(1));
                    self.w("]");
                    let added = &self.buf[checkpoint..];
                    // Reject if: contains \n (leading comment), contains // (inline line
                    // comment that would break the rest of the line), or doesn't fit
                    // (soft width overrides the element count; wrap_width is the ceiling).
                    if !added.contains('\n') && !added.contains("//") && self.inline_fits(a.elems.len(), self.collapse.max_array_elements) {
                        break 'arr;
                    }
                    self.buf.truncate(checkpoint);
                }
                // Expanded form
                self.w("[");
                self.nl();
                self.indent();
                for e in a.elems.iter() {
                    if let Some(e) = e {
                        self.emit_leading_comments(e.span().lo());
                        self.wi();
                        if e.spread.is_some() { self.w("..."); }
                        self.print_expr(&e.expr);
                        self.w(",");
                        self.emit_trailing_comments_bounded(e.expr.span().hi(), a.span.hi());
                    } else {
                        self.wi();
                        self.w(",");
                    }
                    self.nl();
                }
                self.emit_leading_comments(a.span.hi() - BytePos(1));
                self.dedent();
                self.wi();
                self.w("]");
            }
            Expr::Object(o) => 'obj: {
                if o.props.is_empty() {
                    self.w("{}");
                    break 'obj;
                }
                // Object literals: try inline, expand if too many members, too
                // wide, or carrying more than `max_keyvalue_props` named
                // (`key: value`) properties — inline lists of assignments are
                // hard to scan, so they stay one-per-line.
                let kv_props = crate::full::format::keyvalue_prop_count(&o.props);
                if self.collapse.enabled && kv_props <= self.collapse.max_keyvalue_props {
                    let checkpoint = self.buf.len();
                    self.w("{ ");
                    for (i, p) in o.props.iter().enumerate() {
                        if i > 0 { self.w(", "); }
                        let (lo, hi) = match p {
                            PropOrSpread::Spread(s) => (s.span().lo(), s.span().hi()),
                            PropOrSpread::Prop(p) => (p.span().lo(), p.span().hi()),
                        };
                        // Emit leading comments in the trial — if any prop has a leading
                        // comment, the \n forces rollback to expanded form.
                        self.emit_leading_comments(lo);
                        self.print_prop_or_spread(p);
                        // Bounded: don't scan past the closing `}` so statement-level
                        // trailing comments (stored at `;` position) don't force expansion.
                        self.emit_trailing_comments_bounded(hi, o.span.hi());
                    }
                    // Emit closing-brace comments so they force expansion when present
                    self.emit_leading_comments(o.span.hi() - BytePos(1));
                    self.w(" }");
                    let added = &self.buf[checkpoint..];
                    // Reject if: contains \n (leading comment), contains // (inline line
                    // comment that would break the rest of the line), or doesn't fit
                    // (soft width overrides the member count; wrap_width is the ceiling).
                    if !added.contains('\n') && !added.contains("//") && self.inline_fits(o.props.len(), self.collapse.max_object_members) {
                        break 'obj;
                    }
                    self.buf.truncate(checkpoint);
                }
                // Expanded form
                self.w("{");
                self.nl();
                self.indent();
                for p in o.props.iter() {
                    let (lo, hi) = match p {
                        PropOrSpread::Spread(s) => (s.span().lo(), s.span().hi()),
                        PropOrSpread::Prop(p) => (p.span().lo(), p.span().hi()),
                    };
                    self.emit_leading_comments(lo);
                    self.wi();
                    self.print_prop_or_spread(p);
                    self.w(",");
                    self.emit_trailing_comments_bounded(hi, o.span.hi());
                    self.nl();
                }
                self.emit_leading_comments(o.span.hi() - BytePos(1));
                self.dedent();
                self.wi();
                self.w("}");
            }
            Expr::Call(c) => {
                let call_start = self.buf.len();
                self.print_callee(&c.callee);
                if let Some(ta) = &c.type_args {
                    self.w("<");
                    for (i, p) in ta.params.iter().enumerate() { if i > 0 { self.w(", "); } self.print_ts_type(p); }
                    self.w(">");
                }
                self.w("(");
                let args_start = self.buf.len();
                for (i, a) in c.args.iter().enumerate() {
                    if i > 0 { self.w(", "); }
                    if a.spread.is_some() { self.w("..."); }
                    self.print_expr(&a.expr);
                    self.emit_trailing_comments_bounded(a.expr.span().hi(), c.span.hi());
                }
                self.w(")");
                let call_cap = if self.collapse.enabled { self.collapse.max_call_args } else { usize::MAX };
                let has_line_comment = self.buf[args_start..self.buf.len() - 1].split('\n').any(|line| {
                    if let Some(pos) = line.find("//") {
                        if pos == 0 {
                            return true;
                        }
                        let prev = line.as_bytes().get(pos - 1);
                        // Not a comment if preceded by : (URL) or quote (string literal)
                        prev != Some(&b':') && prev != Some(&b'"') && prev != Some(&b'\'')
                    } else {
                        false
                    }
                });
                if !c.args.is_empty() && (has_line_comment || !self.inline_fits(c.args.len(), call_cap)) {
                    self.buf.truncate(call_start);
                    self.print_callee(&c.callee);
                    if let Some(ta) = &c.type_args {
                        self.w("<");
                        for (i, p) in ta.params.iter().enumerate() { if i > 0 { self.w(", "); } self.print_ts_type(p); }
                        self.w(">");
                    }
                    // Line comments inside the arg are fine here: the hug branch
                    // re-prints the object/arrow multi-line, so each `//` comment
                    // lands on its own line. The guard only matters for the inline
                    // expand branch, so it is intentionally not part of `can_hug`.
                    //
                    // A lone function-like argument (arrow or function expression)
                    // always hugs — `foo(() => {\n\t...\n})` — the near-universal
                    // callback style that keeps the callee, params, and opening
                    // brace on one line and closes with `})`. Without this a body
                    // that must break (e.g. it contains a block comment, which is
                    // masked as a `//` placeholder and forces this branch) would
                    // expand the argument onto its own line. Object/array literal
                    // hugging stays opt-in behind `hug_call_args`.
                    let single_arg = c.args.len() == 1 && c.args[0].spread.is_none();
                    let can_hug = single_arg
                        && match &*c.args[0].expr {
                            Expr::Arrow(_) | Expr::Fn(_) => true,
                            Expr::Object(_) | Expr::Array(_) => self.collapse.hug_call_args,
                            _ => false,
                        };
                    // Also hug when the last arg is an arrow/fn in a multi-arg call:
                    // `describe("extract_db_name", () => { ... })` stays on one line
                    // instead of expanding each arg onto its own line.
                    let last_is_arrow = c.args.last().map_or(false, |a| {
                        a.spread.is_none()
                            && matches!(&*a.expr, Expr::Arrow(_) | Expr::Fn(_))
                    });
                    let can_hug_last = c.args.len() > 1 && last_is_arrow;
                    if can_hug {
                        self.w("(");
                        self.print_expr(&c.args[0].expr);
                        self.w(")");
                    } else if can_hug_last {
                        self.w("(");
                        let last_idx = c.args.len() - 1;
                        for (i, a) in c.args.iter().enumerate() {
                            if a.spread.is_some() { self.w("..."); }
                            self.print_expr(&a.expr);
                            self.emit_trailing_comments_bounded(a.expr.span().hi(), c.span.hi());
                            if i < last_idx { self.w(", "); }
                        }
                        self.w(")");
                    } else {
                        self.w("(");
                        self.nl();
                        self.indent();
                        for (i, a) in c.args.iter().enumerate() {
                            self.wi();
                            if a.spread.is_some() { self.w("..."); }
                            self.print_expr(&a.expr);
                            if i < c.args.len() - 1 { self.w(","); }
                            self.emit_trailing_comments_bounded(a.expr.span().hi(), c.span.hi());
                            self.nl();
                        }
                        self.dedent();
                        self.wi();
                        self.w(")");
                    }
                }
            }
            Expr::New(n) => {
                let new_start = self.buf.len();
                self.w("new ");
                self.print_expr(&n.callee);
                if let Some(ta) = &n.type_args {
                    self.w("<");
                    for (i, p) in ta.params.iter().enumerate() { if i > 0 { self.w(", "); } self.print_ts_type(p); }
                    self.w(">");
                }
                self.w("(");
                let args_start = self.buf.len();
                if let Some(args) = &n.args {
                    for (i, a) in args.iter().enumerate() {
                        if i > 0 { self.w(", "); }
                        if a.spread.is_some() { self.w("..."); }
                        self.print_expr(&a.expr);
                        self.emit_trailing_comments_bounded(a.expr.span().hi(), n.span.hi());
                    }
                }
                self.w(")");
                let n_arg_count = n.args.as_ref().map_or(0, |a| a.len());
                let new_cap = if self.collapse.enabled { self.collapse.max_call_args } else { usize::MAX };
                let has_line_comment = self.buf[args_start..self.buf.len() - 1].contains("//");
                if n_arg_count > 0 && (has_line_comment || !self.inline_fits(n_arg_count, new_cap)) {
                    self.buf.truncate(new_start);
                    self.w("new ");
                    self.print_expr(&n.callee);
                    if let Some(ta) = &n.type_args {
                        self.w("<");
                        for (i, p) in ta.params.iter().enumerate() { if i > 0 { self.w(", "); } self.print_ts_type(p); }
                        self.w(">");
                    }
                    self.w("(");
                    self.nl();
                    self.indent();
                    if let Some(args) = &n.args {
                        for a in args.iter() {
                            self.wi();
                            if a.spread.is_some() { self.w("..."); }
                            self.print_expr(&a.expr);
                            self.w(",");
                            self.emit_trailing_comments_bounded(a.expr.span().hi(), n.span.hi());
                            self.nl();
                        }
                    }
                    self.dedent();
                    self.wi();
                    self.w(")");
                }
            }
            Expr::Member(m) => {
                self.print_expr(&m.obj);
                match &m.prop {
                    MemberProp::Ident(id) => { self.w("."); self.w(&*id.sym); }
                    MemberProp::PrivateName(p) => { self.w("."); self.w("#"); self.w(&*p.name); }
                    MemberProp::Computed(c) => { self.w("["); self.print_expr(&c.expr); self.w("]"); }
                }
            }
            Expr::SuperProp(sp) => {
                self.w("super.");
                match &sp.prop {
                    SuperProp::Ident(id) => self.w(&*id.sym),
                    SuperProp::Computed(c) => { self.w("["); self.print_expr(&c.expr); self.w("]"); }
                }
            }
            Expr::Arrow(a) => {
                if a.is_async { self.w("async "); }
                // Always emit params on one line; wrap_long_function_params decides splitting.
                self.w("(");
                for (i, p) in a.params.iter().enumerate() {
                    if i > 0 { self.w(", "); }
                    self.print_pat(p);
                }
                self.w(")");
                if let Some(ret) = &a.return_type {
                    self.w(": ");
                    self.print_ts_type(&ret.type_ann);
                }
                self.w(" => ");
                match a.body.as_ref() {
                    BlockStmtOrExpr::BlockStmt(b) => {
                        if b.stmts.len() == 1 && matches!(&b.stmts[0], Stmt::Expr(_)) {
                            if let Stmt::Expr(e) = &b.stmts[0] {
                                // Only collapse the block if no comments would be lost.
                                // Check leading, closing-brace, and trailing line comments
                                // (matching the same guards in print_block).
                                let has_leading = self
                                    .comments
                                    .get_leading(e.span().lo())
                                    .map_or(false, |c| !c.is_empty());
                                let has_closing = self
                                    .comments
                                    .get_leading(b.span.hi() - BytePos(1))
                                    .map_or(false, |c| !c.is_empty());
                                let has_trailing = self.has_trailing_line_comment(e.span().hi());
                                if has_leading || has_closing || has_trailing {
                                    self.print_block(b);
                                } else {
                                    self.print_expr(&e.expr);
                                }
                            }
                        } else {
                            self.print_block(b);
                        }
                    }
                    BlockStmtOrExpr::Expr(e) => self.print_expr(e),
                }
            }
            Expr::Fn(f) => {
                if f.function.is_async { self.w("async "); }
                self.w("function ");
                if let Some(id) = &f.ident { self.w(&*id.sym); self.w(" "); }
                self.print_fn_sig(&f.function);
                if let Some(body) = &f.function.body {
                    self.w(" ");
                    self.print_block(body);
                }
            }
            Expr::Unary(u) => {
                let op = match u.op {
                    UnaryOp::Minus => "-", UnaryOp::Plus => "+", UnaryOp::Bang => "!",
                    UnaryOp::Tilde => "~", UnaryOp::TypeOf => "typeof ", UnaryOp::Void => "void ",
                    UnaryOp::Delete => "delete ",
                };
                self.w(op);
                self.print_expr(&u.arg);
            }
            Expr::Update(u) => {
                let op = if u.op == UpdateOp::PlusPlus { "++" } else { "--" };
                if u.prefix { self.w(op); self.print_expr(&u.arg); }
                else { self.print_expr(&u.arg); self.w(op); }
            }
            Expr::Bin(b) => {
                self.print_expr(&b.left);
                let op = match b.op {
                    BinaryOp::EqEq => " == ", BinaryOp::NotEq => " != ",
                    BinaryOp::EqEqEq => " === ", BinaryOp::NotEqEq => " !== ",
                    BinaryOp::Lt => " < ", BinaryOp::LtEq => " <= ",
                    BinaryOp::Gt => " > ", BinaryOp::GtEq => " >= ",
                    BinaryOp::LShift => " << ", BinaryOp::RShift => " >> ",
                    BinaryOp::ZeroFillRShift => " >>> ",
                    BinaryOp::Add => " + ", BinaryOp::Sub => " - ",
                    BinaryOp::Mul => " * ", BinaryOp::Div => " / ",
                    BinaryOp::Mod => " % ", BinaryOp::BitAnd => " & ",
                    BinaryOp::BitOr => " | ", BinaryOp::BitXor => " ^ ",
                    BinaryOp::Exp => " ** ",
                    BinaryOp::In => " in ", BinaryOp::InstanceOf => " instanceof ",
                    BinaryOp::LogicalAnd => " && ", BinaryOp::LogicalOr => " || ",
                    BinaryOp::NullishCoalescing => " ?? ",
                };
                self.w(op);
                self.print_expr(&b.right);
            }
            Expr::Cond(c) => {
                self.print_expr(&c.test);
                self.w(" ? ");
                self.print_expr(&c.cons);
                self.w(" : ");
                self.print_expr(&c.alt);
            }
            Expr::Assign(a) => {
                self.print_assign_target(&a.left);
                let op = match a.op {
                    AssignOp::Assign => " = ", AssignOp::AddAssign => " += ",
                    AssignOp::SubAssign => " -= ", AssignOp::MulAssign => " *= ",
                    AssignOp::DivAssign => " /= ", AssignOp::ModAssign => " %= ",
                    AssignOp::LShiftAssign => " <<= ", AssignOp::RShiftAssign => " >>= ",
                    AssignOp::ZeroFillRShiftAssign => " >>>= ",
                    AssignOp::BitOrAssign => " |= ", AssignOp::BitXorAssign => " ^= ",
                    AssignOp::BitAndAssign => " &= ", AssignOp::ExpAssign => " **= ",
                    AssignOp::AndAssign => " &&= ", AssignOp::OrAssign => " ||= ",
                    AssignOp::NullishAssign => " ??= ",
                };
                self.w(op);
                self.print_expr(&a.right);
            }
            Expr::Seq(s) => {
                for (i, e) in s.exprs.iter().enumerate() {
                    if i > 0 { self.w(", "); }
                    self.print_expr(e);
                }
            }
            Expr::Await(a) => { self.w("await "); self.print_expr(&a.arg); }
            Expr::Yield(y) => {
                self.w("yield");
                if let Some(arg) = &y.arg { self.w(" "); self.print_expr(arg); }
            }
            Expr::Paren(p) => { self.w("("); self.print_expr(&p.expr); self.w(")"); }
            Expr::TsAs(a) => { self.print_expr(&a.expr); self.w(" as "); self.print_ts_type(&a.type_ann); }
            Expr::TsNonNull(n) => { self.print_expr(&n.expr); self.w("!"); }
            Expr::TsTypeAssertion(a) => {
                self.w("<"); self.print_ts_type(&a.type_ann); self.w(">"); self.print_expr(&a.expr);
            }
            Expr::TsConstAssertion(a) => {
                self.print_expr(&a.expr); self.w(" as const");
            }
            Expr::TsSatisfies(s) => {
                self.print_expr(&s.expr); self.w(" satisfies "); self.print_ts_type(&s.type_ann);
            }
            Expr::TsInstantiation(i) => {
                self.print_expr(&i.expr);
                self.w("<");
                for (j, a) in i.type_args.params.iter().enumerate() {
                    if j > 0 { self.w(", "); }
                    self.print_ts_type(a);
                }
                self.w(">");
            }
            Expr::MetaProp(m) => {
                match m.kind {
                    MetaPropKind::NewTarget => self.w("new.target"),
                    MetaPropKind::ImportMeta => self.w("import.meta"),
                }
            }
            Expr::OptChain(o) => self.print_opt_chain_base(o),
            Expr::Class(c) => {
                if c.class.is_abstract { self.w("abstract "); }
                self.w("class");
                if let Some(id) = &c.ident { self.w(" "); self.w(&*id.sym); }
                self.print_class(&c.class);
            }
            Expr::PrivateName(p) => { self.w("#"); self.w(&*p.name); }
            Expr::TaggedTpl(t) => {
                self.print_expr(&t.tag);
                self.print_tpl(&t.tpl);
            }
            _ => { self.w("/* expr */"); }
        }
    }

    pub(super) fn print_assign_target(&mut self, target: &AssignTarget) {
        match target {
            AssignTarget::Simple(s) => self.print_simple_assign_target(s),
            AssignTarget::Pat(p) => self.print_assign_target_pat(p),
        }
    }

    fn print_simple_assign_target(&mut self, target: &SimpleAssignTarget) {
        match target {
            SimpleAssignTarget::Ident(i) => {
                self.w(&*i.id.sym);
                if let Some(ty) = &i.type_ann {
                    self.w(": ");
                    self.print_ts_type(&ty.type_ann);
                }
            }
            SimpleAssignTarget::Member(m) => {
                self.print_expr(&m.obj);
                match &m.prop {
                    MemberProp::Ident(id) => { self.w("."); self.w(&*id.sym); }
                    MemberProp::PrivateName(p) => { self.w("."); self.w("#"); self.w(&*p.name); }
                    MemberProp::Computed(c) => { self.w("["); self.print_expr(&c.expr); self.w("]"); }
                }
            }
            SimpleAssignTarget::SuperProp(sp) => {
                self.w("super.");
                match &sp.prop {
                    SuperProp::Ident(id) => self.w(&*id.sym),
                    SuperProp::Computed(c) => { self.w("["); self.print_expr(&c.expr); self.w("]"); }
                }
            }
            SimpleAssignTarget::Paren(p) => {
                self.w("(");
                self.print_expr(&p.expr);
                self.w(")");
            }
            SimpleAssignTarget::OptChain(o) => self.print_opt_chain_base(o),
            SimpleAssignTarget::TsAs(a) => {
                self.print_expr(&a.expr);
                self.w(" as ");
                self.print_ts_type(&a.type_ann);
            }
            SimpleAssignTarget::TsSatisfies(s) => {
                self.print_expr(&s.expr);
                self.w(" satisfies ");
                self.print_ts_type(&s.type_ann);
            }
            SimpleAssignTarget::TsNonNull(n) => {
                self.print_expr(&n.expr);
                self.w("!");
            }
            SimpleAssignTarget::TsTypeAssertion(a) => {
                self.w("<");
                self.print_ts_type(&a.type_ann);
                self.w(">");
                self.print_expr(&a.expr);
            }
            SimpleAssignTarget::TsInstantiation(i) => {
                self.print_expr(&i.expr);
                self.w("<");
                for (j, a) in i.type_args.params.iter().enumerate() {
                    if j > 0 { self.w(", "); }
                    self.print_ts_type(a);
                }
                self.w(">");
            }
            SimpleAssignTarget::Invalid(_) => self.w("<invalid>"),
        }
    }

    fn print_opt_chain_base(&mut self, opt: &OptChainExpr) {
        match opt.base.as_ref() {
            OptChainBase::Member(m) => {
                self.print_expr(&m.obj);
                match &m.prop {
                    MemberProp::Ident(id) => {
                        if opt.optional { self.w("?."); } else { self.w("."); }
                        self.w(&*id.sym);
                    }
                    MemberProp::PrivateName(p) => {
                        if opt.optional { self.w("?."); } else { self.w("."); }
                        self.w("#"); self.w(&*p.name);
                    }
                    MemberProp::Computed(c) => {
                        if opt.optional { self.w("?.["); } else { self.w("["); }
                        self.print_expr(&c.expr); self.w("]");
                    }
                }
            }
            OptChainBase::Call(c) => {
                self.print_expr(&c.callee);
                if opt.optional { self.w("?.("); } else { self.w("("); }
                for (i, a) in c.args.iter().enumerate() {
                    if i > 0 { self.w(", "); }
                    if a.spread.is_some() { self.w("..."); }
                    self.print_expr(&a.expr);
                }
                self.w(")");
            }
        }
    }

    pub(super) fn print_assign_target_pat(&mut self, target: &AssignTargetPat) {
        match target {
            AssignTargetPat::Array(a) => {
                self.w("[");
                for (i, e) in a.elems.iter().enumerate() {
                    if i > 0 { self.w(", "); }
                    if let Some(e) = e { self.print_pat(e); }
                }
                self.w("]");
            }
            AssignTargetPat::Object(o) => {
                self.w("{ ");
                for (i, p) in o.props.iter().enumerate() {
                    if i > 0 { self.w(", "); }
                    self.print_object_pat_prop(p);
                }
                self.w(" }");
            }
            AssignTargetPat::Invalid(_) => self.w("<invalid>"),
        }
    }

    pub(super) fn print_prop_or_spread(&mut self, pos: &PropOrSpread) {
        match pos {
            PropOrSpread::Spread(s) => {
                self.w("...");
                self.print_expr(&s.expr);
            }
            PropOrSpread::Prop(p) => self.print_prop(p),
        }
    }

    pub(super) fn print_callee(&mut self, callee: &Callee) {
        match callee {
            Callee::Super(_) => self.w("super"),
            Callee::Expr(e) => self.print_expr(e),
            Callee::Import(_) => self.w("import"),
        }
    }
}
