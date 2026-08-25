use super::*;

/// The bounded same-crate call graph reachable from `start` (dead helpers are never part of
/// it), used to gate every helper's flow so an unexecuted sibling cannot fabricate evidence.
pub(super) fn collect_reach(start: &str, ev: &CrateEvidence) -> HashSet<String> {
    let mut seen: HashSet<String> = HashSet::new();
    let mut queue: Vec<String> = vec![start.to_string()];
    let mut remaining = GRAPH_MAX_NODES;
    while let Some(n) = queue.pop() {
        if remaining == 0 {
            break;
        }
        remaining -= 1;
        if !seen.insert(n.clone()) {
            continue;
        }
        let Some(f) = ev.fn_item(&n) else {
            continue;
        };
        let mut calls: Vec<String> = Vec::new();
        for stmt in &f.block.stmts {
            collect_stmt_calls(stmt, &ev.fn_names, &mut calls, &mut remaining);
        }
        queue.extend(calls);
    }
    seen
}

/// Collect same-crate function calls out of `stmt` (bounded), so the reach set and the taint
/// memo only ever grow over the same finite graph.
fn collect_stmt_calls(
    stmt: &syn::Stmt,
    fn_names: &HashSet<String>,
    out: &mut Vec<String>,
    remaining: &mut usize,
) {
    match stmt {
        syn::Stmt::Local(l) => {
            if let Some(init) = &l.init {
                collect_expr_calls(&init.expr, fn_names, out, remaining);
            }
        }
        syn::Stmt::Expr(e, _) => collect_expr_calls(e, fn_names, out, remaining),
        _ => {}
    }
}

struct CallCollector<'a> {
    fn_names: &'a HashSet<String>,
    out: &'a mut Vec<String>,
    remaining: &'a mut usize,
}

impl<'ast> syn::visit::Visit<'ast> for CallCollector<'_> {
    fn visit_expr(&mut self, expr: &'ast syn::Expr) {
        if *self.remaining == 0 {
            return;
        }
        *self.remaining -= 1;
        if let syn::Expr::Call(call) = expr {
            self.record_call(call);
        }
        if matches!(expr, syn::Expr::Closure(_) | syn::Expr::Macro(_)) {
            return;
        }
        syn::visit::visit_expr(self, expr);
    }

    fn visit_stmt(&mut self, stmt: &'ast syn::Stmt) {
        match stmt {
            syn::Stmt::Local(local) => {
                if let Some(init) = &local.init {
                    self.visit_expr(&init.expr);
                }
            }
            syn::Stmt::Expr(expr, _) => self.visit_expr(expr),
            _ => {}
        }
    }

    fn visit_pat(&mut self, _pat: &'ast syn::Pat) {}

    fn visit_expr_struct(&mut self, expr: &'ast syn::ExprStruct) {
        for field in &expr.fields {
            self.visit_expr(&field.expr);
        }
    }
}

impl CallCollector<'_> {
    fn record_call(&mut self, call: &syn::ExprCall) {
        let syn::Expr::Path(path) = &*call.func else {
            return;
        };
        let Some(segment) = path.path.segments.first() else {
            return;
        };
        let name = segment.ident.to_string();
        if matches!(segment.arguments, syn::PathArguments::None) && self.fn_names.contains(&name) {
            self.out.push(name);
        }
    }
}

fn collect_expr_calls(
    expr: &syn::Expr,
    fn_names: &HashSet<String>,
    out: &mut Vec<String>,
    remaining: &mut usize,
) {
    use syn::visit::Visit as _;
    CallCollector {
        fn_names,
        out,
        remaining,
    }
    .visit_expr(expr);
}
