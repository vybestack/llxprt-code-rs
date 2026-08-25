use std::collections::{HashMap, HashSet};

#[derive(Clone)]
pub(super) enum LocalKind {
    Unknown,
    Crypto,
}

/// Whether an expression provably resolves to a recognized crypto type: a single-segment
/// local bound to one, a crypto-rooted constructor call, a cast to a crypto type, or a
/// plain wrapping. Anything unresolved is `Unknown`, so a locally defined fake never
/// classifies as crypto-derived.
pub(super) fn infer_expr(
    expression: &syn::Expr,
    locals: &HashMap<String, LocalKind>,
    crypto: &HashSet<String>,
) -> LocalKind {
    match expression {
        syn::Expr::Path(path) if path.qself.is_none() && path.path.segments.len() == 1 => {
            let name = path.path.segments[0].ident.to_string();
            locals.get(&name).cloned().unwrap_or(LocalKind::Unknown)
        }
        syn::Expr::Call(call) => {
            if call_is_crypto(call, locals, crypto) {
                LocalKind::Crypto
            } else {
                LocalKind::Unknown
            }
        }
        syn::Expr::Reference(reference) => infer_expr(&reference.expr, locals, crypto),
        syn::Expr::Group(group) => infer_expr(&group.expr, locals, crypto),
        syn::Expr::Paren(paren) => infer_expr(&paren.expr, locals, crypto),
        syn::Expr::Cast(cast) => {
            if let syn::Type::Path(path) = &*cast.ty {
                if path_root_is_crypto(&path.path, locals, crypto) {
                    return LocalKind::Crypto;
                }
            }
            LocalKind::Unknown
        }
        _ => LocalKind::Unknown,
    }
}

/// Resolve a path root while respecting local bindings that shadow imported crypto names.
pub(super) fn path_root_is_crypto(
    path: &syn::Path,
    locals: &HashMap<String, LocalKind>,
    crypto: &HashSet<String>,
) -> bool {
    let Some(root) = path.segments.first() else {
        return false;
    };
    let name = root.ident.to_string();
    match locals.get(&name) {
        Some(LocalKind::Crypto) => true,
        Some(LocalKind::Unknown) => false,
        None => crypto.contains(&name),
    }
}

pub(super) fn call_is_crypto(
    call: &syn::ExprCall,
    locals: &HashMap<String, LocalKind>,
    crypto: &HashSet<String>,
) -> bool {
    let syn::Expr::Path(path) = &*call.func else {
        return false;
    };
    path_root_is_crypto(&path.path, locals, crypto)
}

/// Record a pattern's bound identifiers in the local shadow-aware type map.
pub(super) fn bind_pattern(
    pattern: &syn::Pat,
    kind: LocalKind,
    locals: &mut HashMap<String, LocalKind>,
) {
    match pattern {
        syn::Pat::Ident(ident) => {
            locals.insert(ident.ident.to_string(), kind);
        }
        syn::Pat::Reference(reference) => bind_pattern(&reference.pat, kind, locals),
        syn::Pat::Paren(paren) => bind_pattern(&paren.pat, kind, locals),
        syn::Pat::Tuple(_) | syn::Pat::Type(_) | syn::Pat::Struct(_) | syn::Pat::Or(_) => {}
        _ => {}
    }
}
