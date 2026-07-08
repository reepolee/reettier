use swc_core::ecma::ast::*;
use super::Printer;

impl<'a> Printer<'a> {
    pub(super) fn print_pat(&mut self, pat: &Pat) {
        match pat {
            Pat::Ident(i) => {
                self.w(&*i.id.sym);
                if i.optional { self.w("?"); }
                if let Some(ty) = &i.type_ann {
                    self.w(": ");
                    self.print_ts_type(&ty.type_ann);
                }
            }
            Pat::Array(a) => {
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
            Pat::Object(o) => {
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
            Pat::Rest(r) => { self.w("..."); self.print_pat(&r.arg); }
            Pat::Assign(a) => { self.print_pat(&a.left); self.w(" = "); self.print_expr(&a.right); }
            Pat::Expr(e) => self.print_expr(e),
            Pat::Invalid(_) => self.w("<invalid>"),
        }
    }

    pub(super) fn print_object_pat_prop(&mut self, prop: &ObjectPatProp) {
        match prop {
            ObjectPatProp::KeyValue(kv) => {
                self.print_prop_name(&kv.key);
                self.w(": ");
                self.print_pat(&kv.value);
            }
            ObjectPatProp::Assign(a) => {
                self.w(&*a.key.sym);
                if let Some(v) = &a.value { self.w(" = "); self.print_expr(v); }
            }
            ObjectPatProp::Rest(r) => { self.w("..."); self.print_pat(&r.arg); }
        }
    }
}
