use super::reach::collect_reach;
use super::*;
mod direction;
mod infer;
mod methods;

pub(super) use direction::OpDir;
use infer::{bind_pattern, call_is_crypto, infer_expr, LocalKind};
pub(super) use methods::{has_unsupported_item_syntax, local_method_names};

/// Whether a value provably derives from a recognized crypto operation result in the
/// analyzed direction. Anything we cannot prove fails closed to `Untainted`, so a synthetic
/// or discarded operation never fabricates evidence. A `Tuple`/`Struct` carries
/// per-element flows so that projection (`.0`, `.field`) selects only the chosen element:
/// a custom sibling element never inherits a tainted sibling.
#[derive(Clone, PartialEq)]
enum Flow {
    Untainted,
    /// A collection constructor or literal proven to contain no values yet.
    Empty,
    /// Data derived from a crypto type but not from the authenticated operation itself,
    /// such as a generated nonce packaged beside ciphertext.
    Crypto,
    Tainted,
    /// A failure-only `Result` outcome such as `Err(..)`. It carries no successful plaintext
    /// or ciphertext, so it does not invalidate a separate operation-derived success path.
    Failure,
    /// Per-element flows of a tuple or array literal. `expr.0` / `expr[0]` projects
    /// exactly the selected element; an out-of-range or unknown index fails closed.
    Tuple(Vec<Flow>),
    /// Per-field flows of a struct literal. `expr.field` projects exactly the named field;
    /// a struct-update (`..base`) copies base's fields, and an unknown shape fails closed.
    Struct(Vec<(String, Flow)>),
}

/// Whether a value is a proven successful operation result: a lone tainted scalar, or an
/// aggregate every one of whose elements is itself a proven operation result. A mixed
/// aggregate (`(custom, real)`) never counts, so a sibling never blesses a custom value.
fn flow_proves_success(f: &Flow) -> bool {
    match f {
        Flow::Tainted => true,
        Flow::Tuple(elems) => !elems.is_empty() && elems.iter().all(flow_proves_success),
        Flow::Struct(fields) => {
            !fields.is_empty() && fields.iter().all(|(_, v)| flow_proves_success(v))
        }
        Flow::Untainted | Flow::Empty | Flow::Crypto | Flow::Failure => false,
    }
}

/// Combine two possible values of the same expression site. Any unproven side fails the whole
/// value (a custom branch never blesses an operation branch); mismatched shapes fail closed.
fn merge_flows(a: Flow, b: Flow) -> Flow {
    use Flow::*;
    match (a, b) {
        (Untainted, _) | (_, Untainted) => Untainted,
        (Tainted, Tainted) => Tainted,
        (Tainted, Failure) | (Failure, Tainted) => Tainted,
        (Crypto, Crypto) | (Crypto, Failure) | (Failure, Crypto) => Crypto,
        (Empty, Empty) | (Empty, Failure) | (Failure, Empty) => Empty,
        (Empty, Tainted)
        | (Tainted, Empty)
        | (Empty, Crypto)
        | (Crypto, Empty)
        | (Tainted, Crypto)
        | (Crypto, Tainted) => Untainted,
        (Failure, Failure) => Failure,
        (Tuple(mut ta), Tuple(tb)) => {
            if ta.len() != tb.len() {
                return Untainted;
            }
            for (x, y) in ta.iter_mut().zip(tb) {
                let merged = std::mem::replace(x, Untainted);
                *x = merge_flows(merged, y);
            }
            Tuple(ta)
        }
        (Struct(mut sa), Struct(sb)) => {
            if sa.len() != sb.len() {
                return Untainted;
            }
            for (k, v) in sa.iter_mut() {
                match sb.iter().find(|(k2, _)| k2 == k) {
                    Some((_, other)) => {
                        let merged = std::mem::replace(v, Untainted);
                        *v = merge_flows(merged, other.clone());
                    }
                    None => return Untainted,
                }
            }
            Struct(sa)
        }
        _ => Untainted,
    }
}

/// Project `member` out of a value's flow: tuple fields project the indexed element and
/// struct fields project the named field. Anything else (unknown shapes, out of range, a
/// member that is not present) fails closed to `Untainted`.
fn project_member(base: &Flow, member: &syn::Member) -> Flow {
    match (base, member) {
        (Flow::Tuple(elems), syn::Member::Unnamed(idx)) => elems
            .get(idx.index as usize)
            .cloned()
            .unwrap_or(Flow::Untainted),
        (Flow::Struct(fields), syn::Member::Named(name)) => fields
            .iter()
            .find(|(k, _)| k == &name.to_string())
            .map(|(_, v)| v.clone())
            .unwrap_or(Flow::Untainted),
        _ => Flow::Untainted,
    }
}

/// Payload-preserving combinators on a `Result`/buffer whose result still carries the
/// operation's value (so `sealed`, `sealed?`, `sealed.map_err(..)` keep the taint).
/// A method not on this list never propagates taint: a `.len()`, `.is_empty()`, or
/// arbitrary fuzzy method is not data-flow evidence.
const PRESERVE_METHODS: &[&str] = &[
    "map_err",
    "unwrap",
    "expect",
    "ok",
    "transpose",
    "clone",
    "to_vec",
    "into_vec",
    "into_boxed_slice",
];

/// Methods that move tainted values into a receiver collection binding. When any argument
/// provably derives from an operation result (e.g. `out.append(&mut sealed)`), the
/// receiver binding becomes tainted too.
const INJECT_METHODS: &[&str] = &[
    "append",
    "extend",
    "extend_from_slice",
    "push",
    "insert",
    "splice",
];

/// Shared per-direction scan state: the bounded budget, the same-crate reach set, and the
/// per-helper taint memo (an in-progress set breaks call cycles conservatively).
struct ScanS<'a, 'b> {
    dir: OpDir,
    ev: &'a CrateEvidence,
    reach: &'a HashSet<String>,
    memo: &'a mut HashMap<String, bool>,
    in_progress: &'a mut HashSet<String>,
    bound: &'b mut usize,
}

/// Whether the receiver expression provably resolves to an allowed crypto type (created by a
/// crypto-rooted constructor or bound to one), which is the only shape allowed to host a
/// recognized authenticated operation.
fn receiver_is_crypto(s: &ScanS, recv: &syn::Expr, locals: &HashMap<String, LocalKind>) -> bool {
    matches!(infer_expr(recv, locals, &s.ev.crypto), LocalKind::Crypto)
}

/// Whether `e` is a literal `false` condition (a provably unreachable branch, including
/// through `(false)`/`{ false }` grouping).
fn cond_is_lit_false(e: &syn::Expr) -> bool {
    match e {
        syn::Expr::Lit(l) => match &l.lit {
            syn::Lit::Bool(b) => !b.value,
            _ => false,
        },
        syn::Expr::Paren(p) => cond_is_lit_false(&p.expr),
        syn::Expr::Group(g) => cond_is_lit_false(&g.expr),
        _ => false,
    }
}

/// Record `pat`'s bound identifiers as carrying `f` in the provenance map. A wildcard
/// (`let _ = ..`) binds nothing, so a discarded operation result can never taint anything.
/// A tuple/struct pattern destructures only the elements it names: a sibling is never tainted
/// by another element, and an unknown or mismatched shape binds nothing (fail closed).
fn bind_flow(pat: &syn::Pat, f: Flow, flow: &mut HashMap<String, Flow>) {
    match pat {
        syn::Pat::Ident(p) => {
            flow.insert(p.ident.to_string(), f);
        }
        syn::Pat::Reference(r) => bind_flow(&r.pat, f, flow),
        syn::Pat::Paren(p) => bind_flow(&p.pat, f, flow),
        syn::Pat::Tuple(tp) => {
            if let Flow::Tuple(elems) = &f {
                if elems.len() == tp.elems.len() {
                    for (el, sub) in tp.elems.iter().zip(elems.iter()) {
                        bind_flow(el, sub.clone(), flow);
                    }
                }
            }
        }
        syn::Pat::Struct(sp) => {
            if let Flow::Struct(fields) = &f {
                for fld in &sp.fields {
                    let key = match &fld.member {
                        syn::Member::Named(id) => id.to_string(),
                        syn::Member::Unnamed(idx) => idx.index.to_string(),
                    };
                    let sub = fields
                        .iter()
                        .find(|(k, _)| *k == key)
                        .map(|(_, v)| v.clone())
                        .unwrap_or(Flow::Untainted);
                    bind_flow(&fld.pat, sub, flow);
                }
            }
        }
        _ => {}
    }
}

/// Whether `name` returns a value that provably derives from a recognized operation result
/// in the scan's direction. Only same-crate helpers inside the bounded reach set are
/// considered (a dead sibling is never analyzed); calls in a cycle resolve to untainted for
/// the inner reference and to their real result for the outermost call.
fn fn_return_tainted(s: &mut ScanS, name: &str) -> bool {
    if let Some(&m) = s.memo.get(name) {
        return m;
    }
    if !s.reach.contains(name) || *s.bound == 0 {
        return false;
    }
    if !s.in_progress.insert(name.to_string()) {
        return false;
    }
    *s.bound -= 1;
    let mut locals: HashMap<String, LocalKind> = HashMap::new();
    let mut flow: HashMap<String, Flow> = HashMap::new();
    let mut all_returns_tainted = true;
    let tail = match s.ev.fn_item(name) {
        Some(f) => scan_block(
            s,
            &f.block,
            &mut locals,
            &mut flow,
            &mut all_returns_tainted,
        ),
        None => Flow::Untainted,
    };
    let tainted = flow_proves_success(&tail) && all_returns_tainted;
    s.in_progress.remove(name);
    s.memo.insert(name.to_string(), tainted);
    tainted
}

/// Scan one expression, returning its provenance. Bindings flow through `locals`/`flow`
/// (shadow-safe, nested blocks clone and discard), and any `return <tainted>` inside
/// reachable code flips `out`.
fn scan_expr(
    scan: &mut ScanS,
    expr: &syn::Expr,
    locals: &mut HashMap<String, LocalKind>,
    flow: &mut HashMap<String, Flow>,
    returns_tainted: &mut bool,
) -> Flow {
    if *scan.bound == 0 {
        return Flow::Untainted;
    }
    *scan.bound -= 1;
    match expr {
        syn::Expr::Path(path) => scan_path(path, flow),
        syn::Expr::Call(call) => scan_call(scan, call, locals, flow, returns_tainted),
        syn::Expr::MethodCall(call) => scan_method_call(scan, call, locals, flow, returns_tainted),
        syn::Expr::Block(block) => {
            scan_nested_block(scan, &block.block, locals, flow, returns_tainted)
        }
        syn::Expr::Unsafe(block) => {
            scan_nested_block(scan, &block.block, locals, flow, returns_tainted)
        }
        syn::Expr::Async(block) => {
            scan_nested_block(scan, &block.block, locals, flow, returns_tainted)
        }
        syn::Expr::Const(block) => {
            scan_nested_block(scan, &block.block, locals, flow, returns_tainted)
        }
        syn::Expr::TryBlock(block) => {
            scan_nested_block(scan, &block.block, locals, flow, returns_tainted)
        }
        syn::Expr::If(branch) => scan_if(scan, branch, locals, flow, returns_tainted),
        syn::Expr::Match(branch) => scan_match(scan, branch, locals, flow, returns_tainted),
        syn::Expr::While(loop_expr) => scan_while(scan, loop_expr, locals, flow, returns_tainted),
        syn::Expr::Loop(loop_expr) => scan_loop(scan, loop_expr, locals, flow, returns_tainted),
        syn::Expr::ForLoop(loop_expr) => scan_for(scan, loop_expr, locals, flow, returns_tainted),
        syn::Expr::Closure(_) => Flow::Untainted,
        syn::Expr::Group(group) => scan_expr(scan, &group.expr, locals, flow, returns_tainted),
        syn::Expr::Paren(paren) => scan_expr(scan, &paren.expr, locals, flow, returns_tainted),
        syn::Expr::Tuple(tuple) => scan_tuple(scan, tuple, locals, flow, returns_tainted),
        syn::Expr::Array(array) => scan_array(scan, array, locals, flow, returns_tainted),
        _ => scan_value_expr(scan, expr, locals, flow, returns_tainted),
    }
}

fn scan_path(path: &syn::ExprPath, flow: &HashMap<String, Flow>) -> Flow {
    if path.qself.is_some() || path.path.segments.len() != 1 {
        return Flow::Untainted;
    }
    let name = path.path.segments[0].ident.to_string();
    flow.get(&name).cloned().unwrap_or(Flow::Untainted)
}

fn scan_call(
    scan: &mut ScanS,
    call: &syn::ExprCall,
    locals: &mut HashMap<String, LocalKind>,
    flow: &mut HashMap<String, Flow>,
    returns_tainted: &mut bool,
) -> Flow {
    if is_empty_collection_call(call) {
        return Flow::Empty;
    }
    if call_is_crypto(call, locals, &scan.ev.crypto) {
        return Flow::Crypto;
    }
    if let Some(name) = simple_call_name(call) {
        if name == "Ok" && call.args.len() == 1 {
            return scan_expr(scan, &call.args[0], locals, flow, returns_tainted);
        }
        if name == "Err" && call.args.len() == 1 {
            scan_expr(scan, &call.args[0], locals, flow, returns_tainted);
            return Flow::Failure;
        }
        if !locals.contains_key(&name)
            && scan.ev.fn_names.contains(&name)
            && fn_return_tainted(scan, &name)
        {
            return Flow::Tainted;
        }
    }
    for argument in &call.args {
        scan_expr(scan, argument, locals, flow, returns_tainted);
    }
    Flow::Untainted
}

fn simple_call_name(call: &syn::ExprCall) -> Option<String> {
    let syn::Expr::Path(path) = &*call.func else {
        return None;
    };
    if path.qself.is_some() || path.path.segments.len() != 1 {
        return None;
    }
    let segment = &path.path.segments[0];
    if !matches!(segment.arguments, syn::PathArguments::None) {
        return None;
    }

    Some(segment.ident.to_string())
}

fn is_empty_collection_call(call: &syn::ExprCall) -> bool {
    let syn::Expr::Path(path) = &*call.func else {
        return false;
    };
    if path.qself.is_some() || !call.args.is_empty() || path.path.segments.len() < 2 {
        return false;
    }
    let mut segments = path.path.segments.iter().rev();
    let Some(method) = segments.next() else {
        return false;
    };
    let Some(collection) = segments.next() else {
        return false;
    };
    method.ident == "new"
        && matches!(
            collection.ident.to_string().as_str(),
            "Vec" | "VecDeque" | "HashMap" | "HashSet" | "BTreeMap" | "BTreeSet"
        )
}

fn scan_method_call(
    scan: &mut ScanS,
    call: &syn::ExprMethodCall,
    locals: &mut HashMap<String, LocalKind>,
    flow: &mut HashMap<String, Flow>,
    returns_tainted: &mut bool,
) -> Flow {
    let name = call.method.to_string();
    if receiver_is_crypto(scan, &call.receiver, locals)
        && scan.dir.has_method(&name)
        && !scan.ev.local_methods.contains(&name)
    {
        return Flow::Tainted;
    }
    let receiver = scan_expr(scan, &call.receiver, locals, flow, returns_tainted);
    let preserved = if PRESERVE_METHODS.contains(&name.as_str()) {
        receiver.clone()
    } else {
        Flow::Untainted
    };
    let mut injected = Flow::Untainted;
    for argument in &call.args {
        injected = scan_expr(scan, argument, locals, flow, returns_tainted);
    }
    if !INJECT_METHODS.contains(&name.as_str()) {
        return preserved;
    }
    let updated = combine_injected_flow(receiver, injected);
    set_receiver_flow(&call.receiver, flow, updated);
    Flow::Untainted
}

fn combine_injected_flow(receiver: Flow, injected: Flow) -> Flow {
    match (receiver, injected) {
        (Flow::Empty, Flow::Crypto) | (Flow::Crypto, Flow::Crypto) => Flow::Crypto,
        (Flow::Empty, Flow::Tainted)
        | (Flow::Crypto, Flow::Tainted)
        | (Flow::Tainted, Flow::Crypto)
        | (Flow::Tainted, Flow::Tainted) => Flow::Tainted,
        _ => Flow::Untainted,
    }
}

fn set_receiver_flow(receiver: &syn::Expr, flow: &mut HashMap<String, Flow>, updated: Flow) {
    let syn::Expr::Path(path) = receiver else {
        return;
    };
    if path.qself.is_none() && path.path.segments.len() == 1 {
        flow.insert(path.path.segments[0].ident.to_string(), updated);
    }
}

fn scan_nested_block(
    scan: &mut ScanS,
    block: &syn::Block,
    locals: &HashMap<String, LocalKind>,
    flow: &HashMap<String, Flow>,
    returns_tainted: &mut bool,
) -> Flow {
    let mut nested_locals = locals.clone();
    let mut nested_flow = flow.clone();
    scan_block(
        scan,
        block,
        &mut nested_locals,
        &mut nested_flow,
        returns_tainted,
    )
}

fn scan_if(
    scan: &mut ScanS,
    branch: &syn::ExprIf,
    locals: &HashMap<String, LocalKind>,
    flow: &HashMap<String, Flow>,
    returns_tainted: &mut bool,
) -> Flow {
    let then_value = if cond_is_lit_false(&branch.cond) {
        Flow::Untainted
    } else {
        scan_nested_block(scan, &branch.then_branch, locals, flow, returns_tainted)
    };
    let Some((_, alternative)) = &branch.else_branch else {
        return Flow::Untainted;
    };
    let mut nested_locals = locals.clone();
    let mut nested_flow = flow.clone();
    let else_value = scan_expr(
        scan,
        alternative,
        &mut nested_locals,
        &mut nested_flow,
        returns_tainted,
    );
    merge_flows(then_value, else_value)
}

fn scan_match(
    scan: &mut ScanS,
    branch: &syn::ExprMatch,
    locals: &mut HashMap<String, LocalKind>,
    flow: &mut HashMap<String, Flow>,
    returns_tainted: &mut bool,
) -> Flow {
    scan_expr(scan, &branch.expr, locals, flow, returns_tainted);
    if branch.arms.is_empty() {
        return Flow::Untainted;
    }
    let mut merged = None;
    let mut all = true;
    let mut any_success = false;
    for arm in &branch.arms {
        if arm.guard.is_some() {
            all = false;
        }
        let mut nested_locals = locals.clone();
        let mut nested_flow = flow.clone();
        let arm_flow = scan_expr(
            scan,
            &arm.body,
            &mut nested_locals,
            &mut nested_flow,
            returns_tainted,
        );
        if flow_proves_success(&arm_flow) {
            any_success = true;
        } else if arm_flow != Flow::Failure {
            all = false;
        }
        merged = Some(match merged {
            None => arm_flow,
            Some(previous) => merge_flows(previous, arm_flow),
        });
    }
    match merged {
        Some(Flow::Untainted) => Flow::Untainted,
        _ if !all => Flow::Untainted,
        _ if any_success => Flow::Tainted,
        _ => Flow::Failure,
    }
}

fn scan_while(
    scan: &mut ScanS,
    loop_expr: &syn::ExprWhile,
    locals: &mut HashMap<String, LocalKind>,
    flow: &mut HashMap<String, Flow>,
    returns_tainted: &mut bool,
) -> Flow {
    scan_expr(scan, &loop_expr.cond, locals, flow, returns_tainted);
    if !cond_is_lit_false(&loop_expr.cond) {
        scan_nested_block(scan, &loop_expr.body, locals, flow, returns_tainted);
    }
    Flow::Untainted
}

fn scan_loop(
    scan: &mut ScanS,
    loop_expr: &syn::ExprLoop,
    locals: &HashMap<String, LocalKind>,
    flow: &HashMap<String, Flow>,
    returns_tainted: &mut bool,
) -> Flow {
    scan_nested_block(scan, &loop_expr.body, locals, flow, returns_tainted);
    Flow::Untainted
}

fn scan_for(
    scan: &mut ScanS,
    loop_expr: &syn::ExprForLoop,
    locals: &mut HashMap<String, LocalKind>,
    flow: &mut HashMap<String, Flow>,
    returns_tainted: &mut bool,
) -> Flow {
    scan_expr(scan, &loop_expr.expr, locals, flow, returns_tainted);
    scan_nested_block(scan, &loop_expr.body, locals, flow, returns_tainted);
    Flow::Untainted
}

fn scan_tuple(
    scan: &mut ScanS,
    tuple: &syn::ExprTuple,
    locals: &mut HashMap<String, LocalKind>,
    flow: &mut HashMap<String, Flow>,
    returns_tainted: &mut bool,
) -> Flow {
    Flow::Tuple(
        tuple
            .elems
            .iter()
            .map(|expr| scan_expr(scan, expr, locals, flow, returns_tainted))
            .collect(),
    )
}

fn scan_array(
    scan: &mut ScanS,
    array: &syn::ExprArray,
    locals: &mut HashMap<String, LocalKind>,
    flow: &mut HashMap<String, Flow>,
    returns_tainted: &mut bool,
) -> Flow {
    let elements: Vec<Flow> = array
        .elems
        .iter()
        .map(|expr| scan_expr(scan, expr, locals, flow, returns_tainted))
        .collect();
    if elements.is_empty() {
        Flow::Empty
    } else {
        Flow::Tuple(elements)
    }
}

fn scan_value_expr(
    scan: &mut ScanS,
    expr: &syn::Expr,
    locals: &mut HashMap<String, LocalKind>,
    flow: &mut HashMap<String, Flow>,
    returns_tainted: &mut bool,
) -> Flow {
    match expr {
        syn::Expr::Repeat(repeat) => scan_repeat(scan, repeat, locals, flow, returns_tainted),
        syn::Expr::Struct(value) => scan_struct(scan, value, locals, flow, returns_tainted),
        syn::Expr::Field(field) => {
            let base = scan_expr(scan, &field.base, locals, flow, returns_tainted);
            project_member(&base, &field.member)
        }
        syn::Expr::Index(index) => scan_index(scan, index, locals, flow, returns_tainted),
        syn::Expr::Reference(value) => scan_expr(scan, &value.expr, locals, flow, returns_tainted),
        syn::Expr::RawAddr(value) => scan_expr(scan, &value.expr, locals, flow, returns_tainted),
        syn::Expr::Unary(value) => scan_expr(scan, &value.expr, locals, flow, returns_tainted),
        syn::Expr::Binary(value) => scan_binary(scan, value, locals, flow, returns_tainted),
        syn::Expr::Cast(value) => scan_expr(scan, &value.expr, locals, flow, returns_tainted),
        syn::Expr::Assign(value) => scan_assign(scan, value, locals, flow, returns_tainted),
        syn::Expr::Range(value) => scan_range(scan, value, locals, flow, returns_tainted),
        syn::Expr::Return(value) => scan_return(scan, value, locals, flow, returns_tainted),
        syn::Expr::Break(value) => {
            scan_optional_expr(scan, value.expr.as_deref(), locals, flow, returns_tainted)
        }
        syn::Expr::Yield(value) => {
            scan_optional_expr(scan, value.expr.as_deref(), locals, flow, returns_tainted)
        }
        syn::Expr::Await(value) => scan_expr(scan, &value.base, locals, flow, returns_tainted),
        syn::Expr::Try(value) => scan_expr(scan, &value.expr, locals, flow, returns_tainted),
        syn::Expr::Let(value) => scan_let(scan, value, locals, flow, returns_tainted),
        _ => Flow::Untainted,
    }
}

fn scan_repeat(
    scan: &mut ScanS,
    repeat: &syn::ExprRepeat,
    locals: &mut HashMap<String, LocalKind>,
    flow: &mut HashMap<String, Flow>,
    returns_tainted: &mut bool,
) -> Flow {
    let value = scan_expr(scan, &repeat.expr, locals, flow, returns_tainted);
    let length = scan_expr(scan, &repeat.len, locals, flow, returns_tainted);
    if value == Flow::Tainted || length == Flow::Tainted {
        Flow::Tainted
    } else {
        Flow::Untainted
    }
}

fn scan_struct(
    scan: &mut ScanS,
    value: &syn::ExprStruct,
    locals: &mut HashMap<String, LocalKind>,
    flow: &mut HashMap<String, Flow>,
    returns_tainted: &mut bool,
) -> Flow {
    let mut fields = match &value.rest {
        Some(rest) => match scan_expr(scan, rest, locals, flow, returns_tainted) {
            Flow::Struct(base) => base,
            _ => Vec::new(),
        },
        None => Vec::new(),
    };
    for field in &value.fields {
        let name = match &field.member {
            syn::Member::Named(ident) => ident.to_string(),
            syn::Member::Unnamed(index) => index.index.to_string(),
        };
        let value = scan_expr(scan, &field.expr, locals, flow, returns_tainted);
        fields.push((name, value));
    }
    if fields.is_empty() {
        Flow::Untainted
    } else {
        Flow::Struct(fields)
    }
}

fn scan_index(
    scan: &mut ScanS,
    index: &syn::ExprIndex,
    locals: &mut HashMap<String, LocalKind>,
    flow: &mut HashMap<String, Flow>,
    returns_tainted: &mut bool,
) -> Flow {
    let base = scan_expr(scan, &index.expr, locals, flow, returns_tainted);
    let syn::Expr::Lit(literal) = &*index.index else {
        return Flow::Untainted;
    };
    let syn::Lit::Int(integer) = &literal.lit else {
        return Flow::Untainted;
    };
    let Ok(position) = integer.base10_parse::<usize>() else {
        return Flow::Untainted;
    };
    let Flow::Tuple(elements) = base else {
        return Flow::Untainted;
    };
    elements.get(position).cloned().unwrap_or(Flow::Untainted)
}

fn scan_binary(
    scan: &mut ScanS,
    binary: &syn::ExprBinary,
    locals: &mut HashMap<String, LocalKind>,
    flow: &mut HashMap<String, Flow>,
    returns_tainted: &mut bool,
) -> Flow {
    let left = scan_expr(scan, &binary.left, locals, flow, returns_tainted);
    let right = scan_expr(scan, &binary.right, locals, flow, returns_tainted);
    if left == Flow::Tainted || right == Flow::Tainted {
        Flow::Tainted
    } else {
        Flow::Untainted
    }
}

fn scan_assign(
    scan: &mut ScanS,
    assign: &syn::ExprAssign,
    locals: &mut HashMap<String, LocalKind>,
    flow: &mut HashMap<String, Flow>,
    returns_tainted: &mut bool,
) -> Flow {
    let value = scan_expr(scan, &assign.right, locals, flow, returns_tainted);
    if let syn::Expr::Path(path) = &*assign.left {
        if path.qself.is_none() && path.path.segments.len() == 1 {
            flow.insert(path.path.segments[0].ident.to_string(), value);
        }
    }
    Flow::Untainted
}

fn scan_range(
    scan: &mut ScanS,
    range: &syn::ExprRange,
    locals: &mut HashMap<String, LocalKind>,
    flow: &mut HashMap<String, Flow>,
    returns_tainted: &mut bool,
) -> Flow {
    let start = range
        .start
        .as_ref()
        .map(|expr| scan_expr(scan, expr, locals, flow, returns_tainted));
    let end = range
        .end
        .as_ref()
        .map(|expr| scan_expr(scan, expr, locals, flow, returns_tainted));
    if start == Some(Flow::Tainted) || end == Some(Flow::Tainted) {
        Flow::Tainted
    } else {
        Flow::Untainted
    }
}

fn scan_return(
    scan: &mut ScanS,
    value: &syn::ExprReturn,
    locals: &mut HashMap<String, LocalKind>,
    flow: &mut HashMap<String, Flow>,
    returns_tainted: &mut bool,
) -> Flow {
    let returned = scan_optional_expr(scan, value.expr.as_deref(), locals, flow, returns_tainted);
    if !flow_proves_success(&returned) && returned != Flow::Failure {
        *returns_tainted = false;
    }
    returned
}

fn scan_optional_expr(
    scan: &mut ScanS,
    expr: Option<&syn::Expr>,
    locals: &mut HashMap<String, LocalKind>,
    flow: &mut HashMap<String, Flow>,
    returns_tainted: &mut bool,
) -> Flow {
    match expr {
        Some(expr) => scan_expr(scan, expr, locals, flow, returns_tainted),
        None => Flow::Untainted,
    }
}

fn scan_let(
    scan: &mut ScanS,
    value: &syn::ExprLet,
    locals: &mut HashMap<String, LocalKind>,
    flow: &mut HashMap<String, Flow>,
    returns_tainted: &mut bool,
) -> Flow {
    let value_flow = scan_expr(scan, &value.expr, locals, flow, returns_tainted);
    let kind = infer_expr(&value.expr, locals, &scan.ev.crypto);
    bind_pattern(&value.pat, kind, locals);
    bind_flow(&value.pat, value_flow, flow);
    Flow::Untainted
}

fn scan_block(
    s: &mut ScanS,
    block: &syn::Block,
    locals: &mut HashMap<String, LocalKind>,
    flow: &mut HashMap<String, Flow>,
    out: &mut bool,
) -> Flow {
    let mut last = Flow::Untainted;
    for stmt in &block.stmts {
        last = scan_stmt(s, stmt, locals, flow, out);
    }
    last
}
fn shadow_item(item: &syn::Item, locals: &mut HashMap<String, LocalKind>) {
    let name = match item {
        syn::Item::Const(item) => Some(&item.ident),
        syn::Item::Enum(item) => Some(&item.ident),
        syn::Item::Fn(item) => Some(&item.sig.ident),
        syn::Item::Mod(item) => Some(&item.ident),
        syn::Item::Static(item) => Some(&item.ident),
        syn::Item::Struct(item) => Some(&item.ident),
        syn::Item::Trait(item) => Some(&item.ident),
        syn::Item::TraitAlias(item) => Some(&item.ident),
        syn::Item::Type(item) => Some(&item.ident),
        syn::Item::Union(item) => Some(&item.ident),
        _ => None,
    };
    if let Some(ident) = name {
        locals.insert(ident.to_string(), LocalKind::Unknown);
    }
}

fn scan_stmt(
    s: &mut ScanS,
    stmt: &syn::Stmt,
    locals: &mut HashMap<String, LocalKind>,
    flow: &mut HashMap<String, Flow>,
    out: &mut bool,
) -> Flow {
    match stmt {
        syn::Stmt::Local(l) => {
            if let Some(init) = &l.init {
                let f = scan_expr(s, &init.expr, locals, flow, out);
                if let Some((_, els)) = &init.diverge {
                    scan_expr(s, els, locals, flow, out);
                }
                let kind = infer_expr(&init.expr, locals, &s.ev.crypto);
                bind_pattern(&l.pat, kind, locals);
                bind_flow(&l.pat, f, flow);
            }
            Flow::Untainted
        }
        syn::Stmt::Expr(e, _) => scan_expr(s, e, locals, flow, out),
        syn::Stmt::Item(item) => {
            shadow_item(item, locals);
            Flow::Untainted
        }
        syn::Stmt::Macro(_) => Flow::Untainted,
    }
}

/// Whether the exported `name` provably returns a value derived from a recognized `dir`
/// operation result reached through its bounded same-crate call graph (constructors, discarded
/// calls, dead helpers, and import/comment/string/macro markers never qualify).
pub(super) fn exported_op_flows_to_return(name: &str, dir: OpDir, ev: &CrateEvidence) -> bool {
    let reach = collect_reach(name, ev);
    if reach.is_empty() {
        return false;
    }
    let mut memo: HashMap<String, bool> = HashMap::new();
    let mut in_progress: HashSet<String> = HashSet::new();
    let mut bound = GRAPH_MAX_NODES * 4;
    let mut s = ScanS {
        dir,
        ev,
        reach: &reach,
        memo: &mut memo,
        in_progress: &mut in_progress,
        bound: &mut bound,
    };
    fn_return_tainted(&mut s, name)
}
