use swc_core::ecma::ast::*;
use swc_core::common::{Spanned, BytePos};
use super::Printer;

impl<'a> Printer<'a> {
    pub(super) fn print_decl(&mut self, decl: &Decl) {
        match decl {
            Decl::Fn(f) => {
                if f.function.is_async { self.w("async "); }
                self.w("function ");
                self.w(&*f.ident.sym);
                self.print_fn_sig(&f.function);
                if let Some(body) = &f.function.body {
                    self.w(" ");
                    self.print_block(body);
                } else {
                    self.w(";");
                }
            }
            Decl::Var(v) => {
                if v.declare { self.w("declare "); }
                match v.kind {
                    VarDeclKind::Var => self.w("var "),
                    VarDeclKind::Let => self.w("let "),
                    VarDeclKind::Const => self.w("const "),
                }
                for (i, d) in v.decls.iter().enumerate() {
                    if i > 0 { self.w(", "); }
                    self.print_var_declarator(d);
                }
                self.w(";");
            }
            Decl::Class(c) => {
                if c.class.is_abstract { self.w("abstract "); }
                self.w("class ");
                self.w(&*c.ident.sym);
                self.print_class(&c.class);
            }
            Decl::TsInterface(i) => {
                if i.declare { self.w("declare "); }
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
                    self.emit_leading_comments(m.span().lo());
                    self.wi();
                    self.print_ts_member(m);
                }
                self.dedent();
                self.wi();
                self.w("}");
            }
            Decl::TsTypeAlias(a) => {
                if a.declare { self.w("declare "); }
                self.w("type ");
                self.w(&*a.id.sym);
                self.w(" = ");
                self.print_ts_type(&a.type_ann);
                self.w(";");
            }
            Decl::TsEnum(e) => {
                self.w("enum ");
                self.w(&*e.id.sym);
                self.w(" {");
                self.nl();
                self.indent();
                for (i, m) in e.members.iter().enumerate() {
                    if i > 0 { self.w(","); self.nl(); }
                    self.emit_leading_comments(m.span.lo());
                    self.wi();
                    match &m.id {
                        TsEnumMemberId::Ident(id) => self.w(&*id.sym),
                        TsEnumMemberId::Str(s) => { self.w("\""); self.w(s.value.as_str().unwrap()); self.w("\""); }
                    }
                    if let Some(init) = &m.init {
                        self.w(" = ");
                        self.print_expr(init);
                    }
                }
                self.nl();
                self.dedent();
                self.wi();
                self.w("}");
            }
            Decl::TsModule(m) => {
                if m.declare { self.w("declare "); }
                if m.global {
                    self.w("global");
                } else {
                    match &m.id {
                        TsModuleName::Ident(id) => { self.w("namespace "); self.w(&*id.sym); }
                        TsModuleName::Str(s) => { self.w("module \""); self.w(s.value.as_str().unwrap()); self.w("\""); }
                    }
                }
                self.w(" {");
                self.nl();
                self.indent();
                if let Some(body) = &m.body {
                    match body {
                        TsNamespaceBody::TsModuleBlock(block) => {
                            for item in &block.body { self.print_module_item(item); }
                        }
                        _ => {}
                    }
                }
                self.dedent();
                self.wi();
                self.w("}");
            }
            Decl::Using(_) => { self.w("using _;"); }
        }
    }

    pub(super) fn print_var_decl(&mut self, v: &VarDecl, add_semi: bool) {
        match v.kind {
            VarDeclKind::Var => self.w("var "),
            VarDeclKind::Let => self.w("let "),
            VarDeclKind::Const => self.w("const "),
        }
        for (i, d) in v.decls.iter().enumerate() {
            if i > 0 { self.w(", "); }
            self.print_var_declarator(d);
        }
        if add_semi { self.w(";"); self.nl(); }
    }

    pub(super) fn print_var_declarator(&mut self, d: &VarDeclarator) {
        self.print_pat(&d.name);
        if let Some(init) = &d.init {
            self.w(" = ");
            self.print_expr(init);
        }
    }

    pub(super) fn print_fn_sig(&mut self, f: &Function) {
        // Always emit params on one line; wrap_long_function_params (which
        // knows max_width) decides whether to split them.
        self.w("(");
        for (i, p) in f.params.iter().enumerate() {
            if i > 0 { self.w(", "); }
            self.print_pat(&p.pat);
        }
        self.w(")");
        if let Some(ret) = &f.return_type {
            self.w(": ");
            self.print_ts_type(&ret.type_ann);
        }
    }

    pub(super) fn print_class(&mut self, cls: &Class) {
        if let Some(super_class) = &cls.super_class {
            self.w(" extends ");
            self.print_expr(super_class);
        }
        if !cls.implements.is_empty() {
            self.w(" implements ");
            for (i, imp) in cls.implements.iter().enumerate() {
                if i > 0 { self.w(", "); }
                self.print_expr(&imp.expr);
                if let Some(tp) = &imp.type_args {
                    self.w("<");
                    for (j, a) in tp.params.iter().enumerate() {
                        if j > 0 { self.w(", "); }
                        self.print_ts_type(a);
                    }
                    self.w(">");
                }
            }
        }
        if cls.body.is_empty() {
            self.w(" {}");
            return;
        }
        self.w(" {");
        self.nl();
        self.indent();
        for member in &cls.body {
            self.emit_leading_comments(member.span().lo());
            self.print_class_member(member);
        }
        self.emit_leading_comments(cls.span.hi() - BytePos(1));
        self.dedent();
        self.wi();
        self.w("}");
    }

    fn print_accessibility(&mut self, acc: &Accessibility) {
        self.w(match acc {
            Accessibility::Public => "public ",
            Accessibility::Protected => "protected ",
            Accessibility::Private => "private ",
        });
    }

    fn print_class_member(&mut self, member: &ClassMember) {
        match member {
            ClassMember::Constructor(c) => {
                self.wi();
                if let Some(acc) = &c.accessibility { self.print_accessibility(acc); }
                self.w("constructor(");
                for (i, p) in c.params.iter().enumerate() {
                    if i > 0 { self.w(", "); }
                    match p {
                        ParamOrTsParamProp::Param(p) => self.print_pat(&p.pat),
                        ParamOrTsParamProp::TsParamProp(p) => {
                            if let Some(acc) = &p.accessibility { self.print_accessibility(acc); }
                            if p.readonly { self.w("readonly "); }
                            match &p.param {
                                TsParamPropParam::Ident(i) => {
                                    self.w(&*i.id.sym);
                                    if i.optional { self.w("?"); }
                                    if let Some(ty) = &i.type_ann {
                                        self.w(": ");
                                        self.print_ts_type(&ty.type_ann);
                                    }
                                }
                                TsParamPropParam::Assign(a) => {
                                    self.print_pat(&a.left);
                                    self.w(" = ");
                                    self.print_expr(&a.right);
                                }
                            }
                        }
                    }
                }
                self.w(")");
                if let Some(body) = &c.body {
                    self.w(" ");
                    self.print_block(body);
                } else {
                    self.w(";");
                }
                self.nl();
            }
            ClassMember::Method(m) => {
                self.wi();
                if m.is_static { self.w("static "); }
                if let Some(acc) = &m.accessibility { self.print_accessibility(acc); }
                if m.is_override { self.w("override "); }
                match m.kind {
                    MethodKind::Getter => self.w("get "),
                    MethodKind::Setter => self.w("set "),
                    MethodKind::Method => {}
                }
                if m.function.is_async { self.w("async "); }
                if m.function.is_generator { self.w("*"); }
                self.print_prop_name(&m.key);
                self.print_fn_sig(&m.function);
                if let Some(body) = &m.function.body {
                    self.w(" ");
                    self.print_block(body);
                } else {
                    self.w(";");
                }
                self.nl();
            }
            ClassMember::PrivateMethod(m) => {
                self.wi();
                if m.is_static { self.w("static "); }
                match m.kind {
                    MethodKind::Getter => self.w("get "),
                    MethodKind::Setter => self.w("set "),
                    MethodKind::Method => {}
                }
                if m.function.is_async { self.w("async "); }
                if m.function.is_generator { self.w("*"); }
                self.w("#");
                self.w(&*m.key.name);
                self.print_fn_sig(&m.function);
                if let Some(body) = &m.function.body {
                    self.w(" ");
                    self.print_block(body);
                } else {
                    self.w(";");
                }
                self.nl();
            }
            ClassMember::ClassProp(p) => {
                self.wi();
                if p.is_static { self.w("static "); }
                if let Some(acc) = &p.accessibility { self.print_accessibility(acc); }
                if p.is_override { self.w("override "); }
                if p.readonly { self.w("readonly "); }
                self.print_prop_name(&p.key);
                if p.definite { self.w("!"); }
                if p.is_optional { self.w("?"); }
                if let Some(ty) = &p.type_ann {
                    self.w(": ");
                    self.print_ts_type(&ty.type_ann);
                }
                if let Some(val) = &p.value {
                    self.w(" = ");
                    self.print_expr(val);
                }
                self.w(";");
                self.nl();
            }
            ClassMember::PrivateProp(p) => {
                self.wi();
                if p.is_static { self.w("static "); }
                if let Some(acc) = &p.accessibility { self.print_accessibility(acc); }
                if p.is_override { self.w("override "); }
                if p.readonly { self.w("readonly "); }
                self.w("#");
                self.w(&*p.key.name);
                if let Some(ty) = &p.type_ann {
                    self.w(": ");
                    self.print_ts_type(&ty.type_ann);
                }
                if let Some(val) = &p.value {
                    self.w(" = ");
                    self.print_expr(val);
                }
                self.w(";");
                self.nl();
            }
            ClassMember::TsIndexSignature(s) => {
                self.wi();
                if s.is_static { self.w("static "); }
                if s.readonly { self.w("readonly "); }
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
                self.nl();
            }
            ClassMember::StaticBlock(b) => {
                self.wi();
                self.w("static ");
                self.print_block(&b.body);
                self.nl();
            }
            ClassMember::Empty(_) | ClassMember::AutoAccessor(_) => {}
        }
    }
}
