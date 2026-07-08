use swc_core::ecma::ast::*;
use super::Printer;

impl<'a> Printer<'a> {
    pub(super) fn print_prop(&mut self, prop: &Prop) {
        match prop {
            Prop::Shorthand(s) => { self.w(&*s.sym); }
            Prop::KeyValue(kv) => {
                self.print_prop_name(&kv.key);
                self.w(": ");
                self.print_expr(&kv.value);
            }
            Prop::Method(m) => {
                if m.function.is_async { self.w("async "); }
                if m.function.is_generator { self.w("*"); }
                self.print_prop_name(&m.key);
                self.print_fn_sig(&m.function);
                self.w(" ");
                if let Some(body) = &m.function.body { self.print_block(body); }
            }
            Prop::Getter(g) => {
                self.w("get ");
                self.print_prop_name(&g.key);
                self.w("() ");
                if let Some(body) = &g.body { self.print_block(body); }
                else { self.nl(); }
            }
            Prop::Setter(s) => {
                self.w("set ");
                self.print_prop_name(&s.key);
                self.w("(");
                self.print_pat(&s.param);
                self.w(") ");
                if let Some(body) = &s.body { self.print_block(body); }
                else { self.nl(); }
            }
            Prop::Assign(a) => {
                self.w(&*a.key.sym);
                self.w(" = ");
                self.print_expr(&a.value);
            }
        }
    }

    pub(super) fn print_prop_name(&mut self, name: &PropName) {
        match name {
            PropName::Ident(id) => self.w(&*id.sym),
            PropName::Str(s) => { self.w("\""); self.w(s.value.as_str().unwrap()); self.w("\""); }
            PropName::Num(n) => self.print_number(n.raw.as_deref(), n.value),
            PropName::BigInt(b) => { self.w(&format!("{}", b.value)); self.w("n"); }
            PropName::Computed(c) => { self.w("["); self.print_expr(&c.expr); self.w("]"); }
        }
    }
}
