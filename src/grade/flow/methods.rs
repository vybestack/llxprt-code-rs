use std::collections::HashSet;

#[derive(Default)]
struct LocalMethodCollector {
    names: HashSet<String>,
}

impl<'ast> syn::visit::Visit<'ast> for LocalMethodCollector {
    fn visit_trait_item_fn(&mut self, method: &'ast syn::TraitItemFn) {
        self.names.insert(method.sig.ident.to_string());
        syn::visit::visit_trait_item_fn(self, method);
    }

    fn visit_impl_item_fn(&mut self, method: &'ast syn::ImplItemFn) {
        self.names.insert(method.sig.ident.to_string());
        syn::visit::visit_impl_item_fn(self, method);
    }
}

pub(in crate::grade) fn local_method_names(file: &syn::File) -> HashSet<String> {
    let mut methods = LocalMethodCollector::default();
    syn::visit::Visit::visit_file(&mut methods, file);
    methods.names
}

#[derive(Default)]
struct UnsupportedSyntaxCollector {
    found: bool,
}

impl<'ast> syn::visit::Visit<'ast> for UnsupportedSyntaxCollector {
    fn visit_attribute(&mut self, attr: &'ast syn::Attribute) {
        let supported = attr.path().is_ident("doc")
            || attr.path().is_ident("allow")
            || attr.path().is_ident("warn")
            || attr.path().is_ident("deny")
            || attr.path().is_ident("forbid")
            || attr.path().is_ident("inline")
            || attr.path().is_ident("cold")
            || attr.path().is_ident("must_use")
            || attr.path().is_ident("deprecated")
            || attr.path().is_ident("track_caller")
            || attr.path().is_ident("repr")
            || attr.path().is_ident("non_exhaustive");
        self.found |= !supported;
        syn::visit::visit_attribute(self, attr);
    }

    fn visit_item_macro(&mut self, item: &'ast syn::ItemMacro) {
        self.found = true;
        syn::visit::visit_item_macro(self, item);
    }
}

/// Whether compiler configuration or item-generating syntax can make the compiled item graph
/// differ from the AST that the flow grader inspects.
pub(in crate::grade) fn has_unsupported_item_syntax(file: &syn::File) -> bool {
    let mut syntax = UnsupportedSyntaxCollector::default();
    syn::visit::Visit::visit_file(&mut syntax, file);
    syntax.found
}
