//! Deterministic AST-based cyclomatic and cognitive complexity metrics.
//!
//! Both metrics are computed in one source-order walk per function, using only the parsed AST,
//! so repeated runs over the same input always produce identical numbers. Nested `fn` items
//! are their own analyzed functions (a metrics walk never descends into them); closures are
//! expressions, so their decisions count inside the enclosing function, matching the LOC rule
//! that a closure is part of its enclosing function.
//!
//! ## Cyclomatic complexity
//!
//! Starts at 1. Increments, in source order:
//! * each `if` and `if let` expression (an `else if` is its own `if` node and counts one);
//! * each loop (`for`, `while`, `while let`, `loop`);
//! * each `match` arm after the first;
//! * each `&&`/`||` short-circuit boolean operator anywhere in the body;
//! * each `match`-arm guard `if` (a guard is an `if` and counts one).
//!
//! ## Cognitive complexity
//!
//! Starts at 0. Structured decisions contribute `1 + nesting`, where nesting is the number of
//! enclosing `if`/loop/`match` constructs; the fixed-cost increments are listed separately:
//! * `if`, `if let`, loops, and `match` contribute `1 + nesting`;
//! * an `else if` decision contributes `1 + nesting` without inheriting extra nesting from the
//!   preceding `if`; decisions inside its body are nested inside that `else if`;
//! * each `match` arm after the first and each match guard contributes `1`;
//! * a boolean operator contributes `1`, unless it is the non-parenthesized left continuation
//!   of the same operator in the same boolean expression (`a && b && c` costs 1,
//!   `a && b || c` costs 2, and `a && (b && c)` costs 2 because the parentheses restart
//!   the sequence);
//! * a function that directly calls itself through a bare name, `self::name`, `Self::name`, or
//!   `self.name()` contributes `1` (recursion).

use syn::visit::{self, Visit};
use syn::{
    BinOp, Expr, ExprBinary, ExprCall, ExprForLoop, ExprIf, ExprLoop, ExprMatch, ExprMethodCall,
    ExprWhile, Item,
};

/// The measured metrics of one function.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Complexity {
    /// Cyclomatic complexity (starts at 1).
    pub cyclomatic: usize,
    /// Cognitive complexity (starts at 0).
    pub cognitive: usize,
}

/// Accumulates both metrics for exactly one function subtree.
pub struct MetricsVisitor {
    /// The per-function accumulated metrics.
    pub complexity: Complexity,
    /// Function name used for direct-recursion detection.
    function_name: Option<String>,
    nesting: usize,
    recursion: bool,
}

impl MetricsVisitor {
    /// Start a fresh metric run for one named function.
    pub fn for_function(name: &str) -> Self {
        MetricsVisitor {
            complexity: Complexity {
                cyclomatic: 1,
                cognitive: 0,
            },
            function_name: Some(name.to_string()),
            nesting: 0,
            recursion: false,
        }
    }

    /// Final metrics with the one-time recursion contribution applied.
    pub fn finish(mut self) -> Complexity {
        if self.recursion {
            self.complexity.cognitive += 1;
        }
        self.complexity
    }

    fn is_direct_recursion(&self, callee: &Expr) -> bool {
        let Some(name) = self.function_name.as_deref() else {
            return false;
        };
        let Expr::Path(path) = callee else {
            return false;
        };
        let segments = &path.path.segments;
        match segments.len() {
            1 => segments[0].ident == name,
            2 => {
                let qualifier = &segments[0].ident;
                (qualifier == "self" || qualifier == "Self") && segments[1].ident == name
            }
            _ => false,
        }
    }

    fn is_self_method_recursion(&self, node: &ExprMethodCall) -> bool {
        let Some(name) = self.function_name.as_deref() else {
            return false;
        };
        node.method == name
            && matches!(&*node.receiver, Expr::Path(path) if path.path.is_ident("self"))
    }
}

impl<'ast> Visit<'ast> for MetricsVisitor {
    fn visit_item(&mut self, item: &'ast Item) {
        // A nested function/impl/trait/mod is its own analyzed unit; it never contributes
        // decisions to the enclosing function. Closures are `Expr::Closure`, not items, so
        // they are still walked here.
        match item {
            Item::Fn(_) | Item::Impl(_) | Item::Trait(_) | Item::Mod(_) => {}
            _ => visit::visit_item(self, item),
        }
    }

    fn visit_expr_if(&mut self, node: &'ast ExprIf) {
        self.complexity.cyclomatic += 1;
        self.complexity.cognitive += 1 + self.nesting;

        self.visit_expr(&node.cond);

        self.nesting += 1;
        self.visit_block(&node.then_branch);
        self.nesting -= 1;

        if let Some((_, else_expr)) = &node.else_branch {
            if matches!(&**else_expr, Expr::If(_)) {
                // `else if`: reached at the SAME nesting, so it contributes `1 + nesting`.
                self.visit_expr(else_expr);
            } else {
                self.nesting += 1;
                self.visit_expr(else_expr);
                self.nesting -= 1;
            }
        }
    }

    fn visit_expr_for_loop(&mut self, node: &'ast ExprForLoop) {
        self.complexity.cyclomatic += 1;
        self.complexity.cognitive += 1 + self.nesting;
        self.nesting += 1;
        self.visit_expr(&node.expr);
        visit::visit_block(self, &node.body);
        self.nesting -= 1;
    }

    fn visit_expr_while(&mut self, node: &'ast ExprWhile) {
        // `while let` is an `ExprWhile` whose condition is `Expr::Let`; both are loops.
        self.complexity.cyclomatic += 1;
        self.complexity.cognitive += 1 + self.nesting;
        self.nesting += 1;
        self.visit_expr(&node.cond);
        visit::visit_block(self, &node.body);
        self.nesting -= 1;
    }

    fn visit_expr_loop(&mut self, node: &'ast ExprLoop) {
        self.complexity.cyclomatic += 1;
        self.complexity.cognitive += 1 + self.nesting;
        self.nesting += 1;
        visit::visit_block(self, &node.body);
        self.nesting -= 1;
    }

    fn visit_expr_match(&mut self, node: &'ast ExprMatch) {
        self.visit_expr(&node.expr);
        self.complexity.cyclomatic += node.arms.len().saturating_sub(1);
        self.complexity.cognitive += 1 + self.nesting;
        self.nesting += 1;
        for (index, arm) in node.arms.iter().enumerate() {
            if index > 0 {
                self.complexity.cognitive += 1;
            }
            if let Some((_, guard)) = &arm.guard {
                // An arm `if` guard is a decision: +1 cyclomatic and +1 cognitive, and any
                // boolean operators inside it are counted by the walk below.
                self.complexity.cyclomatic += 1;
                self.complexity.cognitive += 1;
                self.visit_expr(guard);
            }
            self.visit_pat(&arm.pat);
            self.visit_expr(&arm.body);
        }
        self.nesting -= 1;
    }

    fn visit_expr_binary(&mut self, node: &'ast ExprBinary) {
        match node.op {
            BinOp::And(_) | BinOp::Or(_) => {
                self.complexity.cyclomatic += 1;
                // Boolean sequence transitions: a boolean operator costs 1 unless it is the
                // non-parenthesized left continuation of the same operator in the same boolean
                // expression (parents restart the sequence).
                let same = matches!(&*node.left, Expr::Binary(ExprBinary { op: BinOp::And(_), .. }) if matches!(node.op, BinOp::And(_)))
                    || matches!(&*node.left, Expr::Binary(ExprBinary { op: BinOp::Or(_), .. }) if matches!(node.op, BinOp::Or(_)));
                if !same {
                    self.complexity.cognitive += 1;
                }
            }
            _ => {}
        }
        self.visit_expr(&node.left);
        self.visit_expr(&node.right);
    }

    fn visit_expr_call(&mut self, node: &'ast ExprCall) {
        if self.is_direct_recursion(&node.func) {
            self.recursion = true;
        }
        self.visit_expr(&node.func);
        for arg in &node.args {
            self.visit_expr(arg);
        }
    }

    fn visit_expr_method_call(&mut self, node: &'ast ExprMethodCall) {
        if self.is_self_method_recursion(node) {
            self.recursion = true;
        }
        visit::visit_expr_method_call(self, node);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn metrics(src: &str, name: &str) -> Complexity {
        let file = syn::parse_file(src).expect("fixture must parse");
        let func = file
            .items
            .iter()
            .find_map(|item| match item {
                Item::Fn(func) if func.sig.ident == name => Some(func),
                _ => None,
            })
            .expect("named fixture function");
        let mut m = MetricsVisitor::for_function(name);
        syn::visit::visit_item_fn(&mut m, func);
        m.finish()
    }

    fn is_sig(cyclomatic: usize, cognitive: usize) -> impl Fn(Complexity) -> bool {
        move |c| c.cyclomatic == cyclomatic && c.cognitive == cognitive
    }

    fn expect(name: &str, src: &str, cyclomatic: usize, cognitive: usize) {
        let got = metrics(src, name);
        assert!(
            is_sig(cyclomatic, cognitive)(got),
            "{name}: expected (cyclomatic {cyclomatic}, cognitive {cognitive}), got {got:?}"
        );
    }

    #[test]
    fn empty_body_is_cyclic_1_cognitive_0() {
        expect("noop", "fn noop() {}", 1, 0);
    }

    #[test]
    fn if_elseif_nesting_and_booleans() {
        expect(
            "nest",
            "fn nest(a: bool, b: bool) -> bool { if a { b } else if a && b { true } else { false } }",
            4,
            3,
        );
    }

    #[test]
    fn boolean_sequences_transitions() {
        expect(
            "aaa",
            "fn aaa(a: bool, b: bool, c: bool) -> bool { a && b && c }",
            3,
            1,
        );
        expect(
            "mix",
            "fn mix(a: bool, b: bool, c: bool) -> bool { a && b || c }",
            3,
            2,
        );
        expect(
            "par",
            "fn par(a: bool, b: bool, c: bool) -> bool { a && (b && c) }",
            3,
            2,
        );
    }

    #[test]
    fn match_arms_loops_and_guard() {
        // cyclomatic: base 1 + three match arms (>first -> +2) + one arm guard `if` -> 4.
        // cognitive: match (+1) + arms after first (+2) + guard `if` (+1) -> 4.
        expect(
            "m",
            "fn m(n: u32) -> u32 { match n { (0 | 1) => 1, k if k > 100 => 2, _ => 3 } }",
            1 + 2 + 1,
            1 + 2 + 1,
        );
        expect(
            "loops",
            "fn loops(n: u32) { for i in 0..n { while i < n {} } loop {} }",
            1 + 3,
            // for contributes 1+0, while nests at 1 (1+1), and the sibling loop contributes 1.
            1 + 2 + 1,
        );
        // A guard contains `if` inside; the guard itself is already its own decision.
        expect(
            "g",
            "fn g(n: u32) -> u32 { match n { k if k > 100 => k, _ => 0 } }",
            1 + 1 + 1,
            1 + 1 + 1,
        );
        expect(
            "scrutinee",
            "fn scrutinee(a: bool, b: bool, c: bool) -> bool { match a && b || c { true => true, false => false } }",
            1 + 2 + 1,
            2 + 1 + 1,
        );
    }

    #[test]
    fn direct_recursion_adds_one_cognitive() {
        expect(
            "fact",
            "fn fact(n: u64) -> u64 { if n <= 1 { 1 } else { n * fact(n - 1) } }",
            2,
            2,
        );
        expect("fact", "fn fact() { self::fact(); }", 1, 1);
        expect("fact", "fn fact() { other::fact(); }", 1, 0);
    }

    #[test]
    fn direct_method_recursion_forms_add_one_cognitive() {
        expect("foo", "fn foo() { Self::foo(); }", 1, 1);
        expect("foo", "fn foo() { Type::foo(); }", 1, 0);
        expect("bar", "fn bar() { self.bar(); }", 1, 1);
        expect("bar", "fn bar(other: Thing) { other.bar(); }", 1, 0);
    }

    #[test]
    fn nested_function_is_its_own_unit_not_part_of_parent() {
        expect(
            "outer",
            "fn outer() { fn inner() { if true {} }  if false {} }",
            2,
            1,
        );
    }

    #[test]
    fn iflet_whilelet_and_recursive_chain_are_stable() {
        expect(
            "wl",
            "fn wl(x: Option<u32>) { while let Some(v) = x { if let Some(w) = x { let _ = w; } } }",
            1 + 2,
            1 + 1 + 1,
        );
    }
}
