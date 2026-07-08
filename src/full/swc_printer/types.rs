use swc_core::ecma::ast::*;
use swc_core::common::Spanned;
use super::Printer;

impl<'a> Printer<'a> {
    pub(super) fn print_ts_type(&mut self, ty: &TsType) {
        match ty {
            TsType::TsKeywordType(k) => {
                match k.kind {
                    TsKeywordTypeKind::TsAnyKeyword => self.w("any"),
                    TsKeywordTypeKind::TsNumberKeyword => self.w("number"),
                    TsKeywordTypeKind::TsStringKeyword => self.w("string"),
                    TsKeywordTypeKind::TsBooleanKeyword => self.w("boolean"),
                    TsKeywordTypeKind::TsNullKeyword => self.w("null"),
                    TsKeywordTypeKind::TsUndefinedKeyword => self.w("undefined"),
                    TsKeywordTypeKind::TsVoidKeyword => self.w("void"),
                    TsKeywordTypeKind::TsNeverKeyword => self.w("never"),
                    TsKeywordTypeKind::TsUnknownKeyword => self.w("unknown"),
                    TsKeywordTypeKind::TsSymbolKeyword => self.w("symbol"),
                    TsKeywordTypeKind::TsBigIntKeyword => self.w("bigint"),
                    TsKeywordTypeKind::TsObjectKeyword => self.w("object"),
                    TsKeywordTypeKind::TsIntrinsicKeyword => self.w("intrinsic"),
                }
            }
            TsType::TsTypeRef(r) => {
                self.print_ts_entity_name(&r.type_name);
                if let Some(params) = &r.type_params {
                    self.w("<");
                    for (i, a) in params.params.iter().enumerate() {
                        if i > 0 { self.w(", "); }
                        self.print_ts_type(a);
                    }
                    self.w(">");
                }
            }
            TsType::TsFnOrConstructorType(f) => {
                match f {
                    TsFnOrConstructorType::TsFnType(fn_type) => {
                        self.w("(");
                        for (i, p) in fn_type.params.iter().enumerate() {
                            if i > 0 { self.w(", "); }
                            self.print_ts_fn_param(p);
                        }
                        self.w(") => ");
                        self.print_ts_type(fn_type.type_ann.type_ann.as_ref());
                    }
                    TsFnOrConstructorType::TsConstructorType(ct) => {
                        self.w("new (");
                        for (i, p) in ct.params.iter().enumerate() {
                            if i > 0 { self.w(", "); }
                            self.print_ts_fn_param(p);
                        }
                        self.w(") => ");
                        self.print_ts_type(ct.type_ann.type_ann.as_ref());
                    }
                }
            }
            TsType::TsArrayType(a) => { self.print_ts_type(&a.elem_type); self.w("[]"); }
            TsType::TsTupleType(t) => {
                self.w("[");
                for (i, e) in t.elem_types.iter().enumerate() {
                    if i > 0 { self.w(", "); }
                    self.print_ts_type(&e.ty);
                }
                self.w("]");
            }
            TsType::TsUnionOrIntersectionType(u) => {
                match u {
                    TsUnionOrIntersectionType::TsUnionType(u) => {
                        for (i, t) in u.types.iter().enumerate() {
                            if i > 0 { self.w(" | "); }
                            self.print_ts_type(t);
                        }
                    }
                    TsUnionOrIntersectionType::TsIntersectionType(i) => {
                        for (i, t) in i.types.iter().enumerate() {
                            if i > 0 { self.w(" & "); }
                            self.print_ts_type(t);
                        }
                    }
                }
            }
            TsType::TsConditionalType(c) => {
                self.print_ts_type(&c.check_type);
                self.w(" extends ");
                self.print_ts_type(&c.extends_type);
                self.w(" ? ");
                self.print_ts_type(&c.true_type);
                self.w(" : ");
                self.print_ts_type(&c.false_type);
            }
            TsType::TsInferType(i) => { self.w("infer "); self.w(&*i.type_param.name.sym); }
            TsType::TsParenthesizedType(p) => { self.w("("); self.print_ts_type(&p.type_ann); self.w(")"); }
            TsType::TsTypeOperator(o) => {
                match o.op {
                    TsTypeOperatorOp::KeyOf => self.w("keyof "),
                    TsTypeOperatorOp::Unique => self.w("unique "),
                    TsTypeOperatorOp::ReadOnly => self.w("readonly "),
                }
                self.print_ts_type(&o.type_ann);
            }
            TsType::TsIndexedAccessType(i) => {
                self.print_ts_type(&i.obj_type);
                self.w("[");
                self.print_ts_type(&i.index_type);
                self.w("]");
            }
            TsType::TsMappedType(m) => {
                self.w("{");
                if m.readonly.is_some() { self.w(" readonly "); }
                self.w("[");
                self.w(&*m.type_param.name.sym);
                if let Some(c) = &m.type_param.constraint { self.w(" in "); self.print_ts_type(c); }
                self.w("]");
                if m.optional.is_some() { self.w("?"); }
                if let Some(ty) = &m.type_ann { self.w(": "); self.print_ts_type(ty); }
                self.w("}");
            }
            TsType::TsTypeLit(l) => 'ty: {
                let members = &l.members;
                let any_trailing_line = members.iter().any(|m| self.has_trailing_line_comment(m.span().hi));
                if !any_trailing_line && self.collapse.enabled {
                    let checkpoint = self.buf.len();
                    self.w("{ ");
                    for (i, m) in members.iter().enumerate() {
                        if i > 0 { self.w(" "); }
                        self.emit_leading_comments(m.span().lo);
                        self.print_ts_member_inline(m);
                    }
                    self.w(" }");
                    let added = &self.buf[checkpoint..];
                    if !added.contains('\n') && self.inline_fits(members.len(), self.collapse.max_type_members) {
                        break 'ty;
                    }
                    self.buf.truncate(checkpoint);
                }
                // Expanded form
                self.w("{");
                self.nl();
                self.indent();
                for m in members {
                    self.emit_leading_comments(m.span().lo);
                    self.wi();
                    self.print_ts_member(m);
                }
                self.dedent();
                self.wi();
                self.w("}");
            }
            TsType::TsLitType(l) => {
                match &l.lit {
                    TsLit::Str(s) => { self.w("\""); self.w(s.value.as_str().unwrap()); self.w("\""); }
                    TsLit::Number(n) => self.print_number(n.raw.as_deref(), n.value),
                    TsLit::Bool(b) => self.w(if b.value { "true" } else { "false" }),
                    TsLit::BigInt(b) => { self.w(&format!("{}", b.value)); self.w("n"); }
                    TsLit::Tpl(t) => {
                        self.w("`");
                        for i in 0..t.quasis.len() {
                            if let Some(q) = t.quasis.get(i) { self.w(&*q.raw); }
                        }
                        self.w("`");
                    }
                }
            }
            TsType::TsThisType(_) => self.w("this"),
            TsType::TsTypePredicate(p) => {
                if p.asserts { self.w("asserts "); }
                match &p.param_name {
                    TsThisTypeOrIdent::TsThisType(_) => self.w("this"),
                    TsThisTypeOrIdent::Ident(id) => self.w(&*id.sym),
                }
                if let Some(ann) = &p.type_ann {
                    self.w(" is ");
                    self.print_ts_type(&ann.type_ann);
                }
            }
            TsType::TsImportType(t) => {
                self.w("import(\"");
                self.w(t.arg.value.as_str().unwrap());
                self.w("\")");
                if let Some(qualifier) = &t.qualifier {
                    self.w(".");
                    self.print_ts_entity_name(qualifier);
                }
                if let Some(type_args) = &t.type_args {
                    self.w("<");
                    for (i, a) in type_args.params.iter().enumerate() {
                        if i > 0 { self.w(", "); }
                        self.print_ts_type(a);
                    }
                    self.w(">");
                }
            }
            TsType::TsOptionalType(o) => { self.print_ts_type(&o.type_ann); self.w("?"); }
            TsType::TsRestType(r) => { self.w("..."); self.print_ts_type(&r.type_ann); }
            TsType::TsTypeQuery(q) => {
                self.w("typeof ");
                match &q.expr_name {
                    TsTypeQueryExpr::TsEntityName(name) => self.print_ts_entity_name(name),
                    TsTypeQueryExpr::Import(i) => {
                        self.w("import(\"");
                        self.w(i.arg.value.as_str().unwrap());
                        self.w("\")");
                    }
                }
                if let Some(type_args) = &q.type_args {
                    self.w("<");
                    for (j, a) in type_args.params.iter().enumerate() {
                        if j > 0 { self.w(", "); }
                        self.print_ts_type(a);
                    }
                    self.w(">");
                }
            }
        }
    }

    pub(super) fn print_ts_member(&mut self, m: &TsTypeElement) {
        let hi = m.span().hi;
        match m {
            TsTypeElement::TsPropertySignature(p) => {
                if p.readonly { self.w("readonly "); }
                self.print_expr(&p.key);
                if p.optional { self.w("?"); }
                if let Some(ty) = &p.type_ann {
                    self.w(": ");
                    self.print_ts_type(&ty.type_ann);
                }
                self.w(";");
                self.emit_trailing_comments(hi);
                self.nl();
            }
            TsTypeElement::TsMethodSignature(sig) => {
                self.print_expr(&sig.key);
                self.w("(");
                for (i, p) in sig.params.iter().enumerate() {
                    if i > 0 { self.w(", "); }
                    self.print_ts_fn_param(p);
                }
                self.w(")");
                if let Some(ret) = &sig.type_ann {
                    self.w(": ");
                    self.print_ts_type(&ret.type_ann);
                }
                self.w(";");
                self.emit_trailing_comments(hi);
                self.nl();
            }
            TsTypeElement::TsCallSignatureDecl(c) => {
                self.w("(");
                for (i, p) in c.params.iter().enumerate() {
                    if i > 0 { self.w(", "); }
                    self.print_ts_fn_param(p);
                }
                self.w(")");
                if let Some(ret) = &c.type_ann {
                    self.w(": ");
                    self.print_ts_type(&ret.type_ann);
                }
                self.w(";");
                self.emit_trailing_comments(hi);
                self.nl();
            }
            TsTypeElement::TsConstructSignatureDecl(c) => {
                self.w("new (");
                for (i, p) in c.params.iter().enumerate() {
                    if i > 0 { self.w(", "); }
                    self.print_ts_fn_param(p);
                }
                self.w(")");
                if let Some(ret) = &c.type_ann {
                    self.w(": ");
                    self.print_ts_type(&ret.type_ann);
                }
                self.w(";");
                self.emit_trailing_comments(hi);
                self.nl();
            }
            TsTypeElement::TsIndexSignature(s) => {
                self.w("[");
                for (i, p) in s.params.iter().enumerate() {
                    if i > 0 { self.w(", "); }
                    self.print_ts_fn_param(p);
                }
                self.w("]");
                if let Some(ty) = &s.type_ann {
                    self.w(": ");
                    self.print_ts_type(&ty.type_ann);
                }
                self.w(";");
                self.emit_trailing_comments(hi);
                self.nl();
            }
            _ => { self.w("// ts member\n"); }
        }
    }

    pub(super) fn print_ts_fn_param(&mut self, param: &TsFnParam) {
        match param {
            TsFnParam::Ident(i) => {
                self.w(&*i.id.sym);
                if i.optional { self.w("?"); }
                if let Some(ty) = &i.type_ann {
                    self.w(": ");
                    self.print_ts_type(&ty.type_ann);
                }
            }
            TsFnParam::Array(a) => {
                self.w("[");
                for (i, e) in a.elems.iter().enumerate() {
                    if i > 0 { self.w(", "); }
                    if let Some(e) = e { self.print_pat(e); }
                }
                self.w("]");
                if a.optional { self.w("?"); }
                if let Some(ty) = &a.type_ann {
                    self.w(": ");
                    self.print_ts_type(&ty.type_ann);
                }
            }
            TsFnParam::Rest(r) => {
                self.w("...");
                self.print_pat(&*r.arg);
                if let Some(ty) = &r.type_ann {
                    self.w(": ");
                    self.print_ts_type(&ty.type_ann);
                }
            }
            TsFnParam::Object(o) => {
                self.w("{ ");
                for (i, p) in o.props.iter().enumerate() {
                    if i > 0 { self.w(", "); }
                    self.print_object_pat_prop(p);
                }
                self.w(" }");
                if o.optional { self.w("?"); }
                if let Some(ty) = &o.type_ann {
                    self.w(": ");
                    self.print_ts_type(&ty.type_ann);
                }
            }
        }
    }

    /// Print a TS type member inline (no trailing semicolon/newline, separated by spaces).
    fn print_ts_member_inline(&mut self, m: &TsTypeElement) {
        match m {
            TsTypeElement::TsPropertySignature(p) => {
                if p.readonly { self.w("readonly "); }
                self.print_expr(&p.key);
                if p.optional { self.w("?"); }
                if let Some(ty) = &p.type_ann {
                    self.w(": ");
                    self.print_ts_type(&ty.type_ann);
                }
                self.w(";");
            }
            TsTypeElement::TsMethodSignature(m) => {
                self.print_expr(&m.key);
                self.w("(");
                for (i, p) in m.params.iter().enumerate() {
                    if i > 0 { self.w(", "); }
                    self.print_ts_fn_param(p);
                }
                self.w(")");
                if let Some(ret) = &m.type_ann {
                    self.w(": ");
                    self.print_ts_type(&ret.type_ann);
                }
                self.w(";");
            }
            TsTypeElement::TsCallSignatureDecl(c) => {
                self.w("(");
                for (i, p) in c.params.iter().enumerate() {
                    if i > 0 { self.w(", "); }
                    self.print_ts_fn_param(p);
                }
                self.w(")");
                if let Some(ret) = &c.type_ann {
                    self.w(": ");
                    self.print_ts_type(&ret.type_ann);
                }
                self.w(";");
            }
            TsTypeElement::TsConstructSignatureDecl(c) => {
                self.w("new (");
                for (i, p) in c.params.iter().enumerate() {
                    if i > 0 { self.w(", "); }
                    self.print_ts_fn_param(p);
                }
                self.w(")");
                if let Some(ret) = &c.type_ann {
                    self.w(": ");
                    self.print_ts_type(&ret.type_ann);
                }
                self.w(";");
            }
            TsTypeElement::TsIndexSignature(s) => {
                self.w("[");
                for (i, p) in s.params.iter().enumerate() {
                    if i > 0 { self.w(", "); }
                    self.print_ts_fn_param(p);
                }
                self.w("]");
                if let Some(ty) = &s.type_ann {
                    self.w(": ");
                    self.print_ts_type(&ty.type_ann);
                }
                self.w(";");
            }
            _ => { self.w("// ts member;"); }
        }
    }

    pub(super) fn print_ts_entity_name(&mut self, name: &TsEntityName) {
        match name {
            TsEntityName::Ident(id) => self.w(&*id.sym),
            TsEntityName::TsQualifiedName(q) => {
                self.print_ts_entity_name(&q.left);
                self.w(".");
                self.w(&*q.right.sym);
            }
        }
    }
}
