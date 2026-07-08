use swc_core::ecma::ast::*;
use super::Printer;

impl<'a> Printer<'a> {
    pub(super) fn print_lit(&mut self, lit: &Lit) {
        match lit {
            Lit::Str(s) => { self.print_str_lit(s.value.as_str().unwrap()); }
            Lit::Num(n) => self.print_number(n.raw.as_deref(), n.value),
            Lit::Bool(b) => self.w(if b.value { "true" } else { "false" }),
            Lit::Null(_) => self.w("null"),
            Lit::BigInt(b) => { self.w(&format!("{}", b.value)); self.w("n"); }
            Lit::Regex(r) => {
                self.w("/"); self.w(r.exp.as_str()); self.w("/");
                if !r.flags.as_str().is_empty() { self.w(r.flags.as_str()); }
            }
            _ => self.w("<lit>"),
        }
    }

    pub(super) fn print_number(&mut self, raw: Option<&str>, value: f64) {
        if let Some(r) = raw {
            self.w(r);
        } else if value.fract() == 0.0 && value < 1e15 {
            self.w(&format!("{}", value as i64));
        } else {
            self.w(&format!("{}", value));
        }
    }

    pub(super) fn print_tpl(&mut self, tpl: &Tpl) {
        self.w("`");
        for i in 0..tpl.quasis.len() {
            if let Some(q) = tpl.quasis.get(i) { self.w(&*q.raw); }
            if let Some(e) = tpl.exprs.get(i) {
                self.w("${");
                self.print_expr(e);
                self.w("}");
            }
        }
        self.w("`");
    }
}
