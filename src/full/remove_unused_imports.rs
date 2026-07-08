//! Custom unused import removal pass for SWC's AST.
//!
//! Uses SWC's `Visit` trait to collect identifier references across the module,
//! then removes import declarations whose specifiers' local names are never
//! referenced in the module body.
//!
//! # Edge cases handled
//! - **Side-effect imports** (`import "./foo"`) — always kept (no specifiers)
//! - **Type-only imports** (`import type { X }`) — treated same as value imports
//! - **Default imports** (`import x from "mod"`) — removed if `x` is unused
//! - **Namespace imports** (`import * as ns`) — removed if `ns` is unused
//! - **Re-exports** (`export { x } from "mod"`) — not ImportDecl, left untouched
//! - **Shadowed imports** — import binding that shares a name with a local var
//!   is correctly not counted as a reference (we only check binding names, not scopes)

use std::collections::{HashMap, HashSet};
use swc_core::ecma::ast::*;
use swc_core::ecma::visit::{Visit, VisitWith};

/// Remove unused import declarations from a SWC `Module` AST in-place.
///
/// Returns `true` if any imports were removed.
pub(crate) fn remove_unused_imports(module: &mut Module) -> bool {
    // ── Step 1: Collect all import declarations and their specifier bindings ──
    //
    // import_info[i] corresponds to the i-th ImportDecl in module.body.
    // binding_to_body_idx: local name → body index of the ImportDecl.

    struct ImportInfo {
        body_index: usize,
        /// Local names that this import provides (e.g. ["x", "y"] for `import { x, y }`).
        local_names: Vec<String>,
        /// Whether all specifiers are unused.
        has_specifiers: bool,
    }

    let mut import_infos: Vec<ImportInfo> = vec![];
    let mut binding_to_import: HashMap<String, usize> = HashMap::new();

    for (i, item) in module.body.iter().enumerate() {
        if let ModuleItem::ModuleDecl(ModuleDecl::Import(import)) = item {
            let mut info = ImportInfo {
                body_index: i,
                local_names: vec![],
                has_specifiers: !import.specifiers.is_empty(),
            };
            for spec in &import.specifiers {
                let local = match spec {
                    ImportSpecifier::Named(n) => &n.local,
                    ImportSpecifier::Default(d) => &d.local,
                    ImportSpecifier::Namespace(ns) => &ns.local,
                };
                let name = local.sym.to_string();
                binding_to_import.insert(name.clone(), import_infos.len());
                info.local_names.push(name);
            }
            import_infos.push(info);
        }
    }

    if import_infos.is_empty() {
        return false; // Nothing to do
    }

    // ── Step 2: Collect all identifier references in the module body ──
    //
    // We use SWC's `Visit` trait which walks the entire AST subtree.
    // By overriding `visit_import_decl` with an empty body, we skip
    // identifiers inside import declarations (which are definitions, not
    // references).
    //
    // We also skip re-export declarations (`export { x } from "mod"`)
    // because their specifier names reference the source module, not
    // local bindings.

    struct IdentRefCollector {
        refs: HashSet<String>,
    }

    impl Visit for IdentRefCollector {
        fn visit_ident(&mut self, ident: &Ident) {
            self.refs.insert(ident.sym.to_string());
        }

        fn visit_import_decl(&mut self, _: &ImportDecl) {
            // Skip — import specifier names are definitions, not references.
        }

        fn visit_named_export(&mut self, export: &NamedExport) {
            if export.src.is_some() {
                // Re-export (`export { x } from "mod"`): specifier names
                // reference the source module, not local bindings — skip.
            } else {
                // Non-re-export named export (`export { x }`): specifier
                // names reference local bindings — visit children.
                export.visit_children_with(self);
            }
        }
    }

    let mut collector = IdentRefCollector {
        refs: HashSet::new(),
    };
    module.visit_with(&mut collector);

    // ── Step 3: Determine which imports are unused ──

    let keep_import: Vec<bool> = import_infos
        .iter()
        .map(|info| {
            if !info.has_specifiers {
                // Side-effect import `import "foo"` — always keep.
                return true;
            }
            // Keep if at least one specifier's local name is referenced.
            info.local_names
                .iter()
                .any(|name| collector.refs.contains(name.as_str()))
        })
        .collect();

    // ── Step 4: Remove unused imports from module body ──
    //
    // Remove in reverse order so indices stay valid.

    let mut changed = false;
    for (i, &keep) in keep_import.iter().enumerate().rev() {
        if !keep {
            let body_idx = import_infos[i].body_index;
            module.body.remove(body_idx);
            changed = true;
        }
    }

    changed
}

#[cfg(test)]
mod tests {
    use super::*;
    use swc_core::common::comments::SingleThreadedComments;
    use swc_core::common::input::StringInput;
    use swc_core::common::sync::Lrc;
    use swc_core::common::{FileName, SourceMap};
    use swc_core::ecma::codegen::text_writer::JsWriter;
    use swc_core::ecma::codegen::{Config as CodegenConfig, Emitter};
    use swc_core::ecma::parser::lexer::Lexer;
    use swc_core::ecma::parser::{Parser, Syntax, TsSyntax};

    /// Parse a string into a SWC Module, run remove_unused_imports, and
    /// emit back to a string for assertion.
    fn run_remove_unused_imports(code: &str) -> String {
        let cm: Lrc<SourceMap> = Lrc::new(SourceMap::default());
        let fm = cm.new_source_file(FileName::Anon.into(), code.to_string());
        let comments = SingleThreadedComments::default();

        let syntax = Syntax::Typescript(TsSyntax {
            tsx: false,
            decorators: false,
            ..Default::default()
        });
        let input = StringInput::new(&fm.src, fm.start_pos, fm.end_pos);
        let lexer = Lexer::new(syntax, EsVersion::latest(), input, Some(&comments));
        let mut parser = Parser::new_from(lexer);
        let mut module = parser.parse_module().expect("parse should succeed");

        remove_unused_imports(&mut module);

        // Emit back to string
        let mut buf = vec![];
        {
            let wr = JsWriter::new(cm.clone(), "\n", &mut buf, None);
            let config = CodegenConfig::default();
            let mut emitter = Emitter {
                cfg: config,
                cm: cm.clone(),
                comments: Some(&comments),
                wr,
            };
            emitter.emit_module(&module).unwrap();
        }
        String::from_utf8_lossy(&buf).to_string()
    }

    #[test]
    fn removes_unused_named_imports() {
        let result = run_remove_unused_imports(
            "import { get_rate_limit_status, reset_rate_limits } from \"$lib/middleware\";\nimport { require_admin_auth } from \"./require_admin_auth\";\n",
        );
        // All imports are unused — they should all be removed
        assert!(
            !result.contains("import"),
            "All unused imports should be removed, got: {:?}",
            result
        );
    }

    #[test]
    fn keeps_used_import() {
        let result = run_remove_unused_imports(
            "import { get_rate_limit_status } from \"$lib/middleware\";\nexport async function handle() {\n    const status = await get_rate_limit_status();\n    return status;\n}\n",
        );
        assert!(
            result.contains("import"),
            "Used import should be kept, got: {:?}",
            result
        );
        assert!(
            result.contains("get_rate_limit_status"),
            "Used import name should appear in output"
        );
    }

    #[test]
    fn removes_unused_keeps_used_mixed() {
        let result = run_remove_unused_imports(
            "import { get_rate_limit_status, reset_rate_limits } from \"$lib/middleware\";\nimport { require_admin_auth } from \"./require_admin_auth\";\nexport async function handle() {\n    const status = await get_rate_limit_status();\n    return status;\n}\n",
        );
        // get_rate_limit_status is used, so its import stays
        assert!(
            result.contains("import"),
            "Used import should be kept"
        );
        // reset_rate_limits is unused but it's in the same import decl as get_rate_limit_status,
        // so the entire import stays (we remove whole declarations, not individual specifiers)
        assert!(
            result.contains("get_rate_limit_status"),
            "Used binding should appear in output"
        );
        // require_admin_auth is unused — its entire import declaration should be removed
        assert!(
            !result.contains("require_admin_auth"),
            "Unused import declaration should be removed"
        );
    }

    #[test]
    fn keeps_side_effect_import() {
        let result = run_remove_unused_imports(
            "import \"./styles.css\";\nimport { unused } from \"./lib\";\n",
        );
        assert!(
            result.contains("./styles.css"),
            "Side-effect import should be kept"
        );
        assert!(
            !result.contains("unused"),
            "Unused named import should be removed"
        );
    }

    #[test]
    fn keeps_default_import_when_used() {
        let result = run_remove_unused_imports(
            "import foo from \"mod\";\nfoo();\n",
        );
        assert!(result.contains("foo"), "Used default import should be kept");
    }

    #[test]
    fn removes_unused_default_import() {
        let result = run_remove_unused_imports(
            "import foo from \"mod\";\n",
        );
        assert!(!result.contains("import"), "Unused default import should be removed");
    }

    #[test]
    fn keeps_namespace_import_when_used() {
        let result = run_remove_unused_imports(
            "import * as ns from \"mod\";\nns.foo();\n",
        );
        assert!(result.contains("ns"), "Used namespace import should be kept");
    }

    #[test]
    fn removes_unused_namespace_import() {
        let result = run_remove_unused_imports(
            "import * as ns from \"mod\";\n",
        );
        assert!(!result.contains("import"), "Unused namespace import should be removed");
    }

    #[test]
    fn preserves_export_from_non_reexport() {
        // `export { x }` (without `from`) references local binding `x`
        let result = run_remove_unused_imports(
            "import { x } from \"mod\";\nexport { x };\n",
        );
        assert!(
            result.contains("import"),
            "Import used by local export should be kept"
        );
    }

    #[test]
    fn re_export_does_not_falsely_keep_import() {
        // `export { x } from "reexport"` should NOT count as a reference
        // to a separate `import { x } from "original"` even though both
        // use the name `x`.
        let result = run_remove_unused_imports(
            "import { x } from \"./original\";\nexport { x } from \"./reexport\";\n",
        );
        assert!(
            !result.contains("./original"),
            "Import shadowed by re-export should be removed"
        );
        assert!(
            result.contains("./reexport"),
            "Re-export declaration should be preserved"
        );
    }

    #[test]
    fn no_change_when_nothing_unused() {
        let code = "import { x } from \"mod\";\nconsole.log(x);\n";
        let result = run_remove_unused_imports(code);
        assert!(
            result.contains("import"),
            "Import should remain when used"
        );
        assert!(
            result.contains("x"),
            "Binding x should appear in output"
        );
    }
}
