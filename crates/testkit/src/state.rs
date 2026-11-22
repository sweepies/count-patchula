//! `tests/state-versions.tsv` holds a row for every code-versioned `State`
//! and `tests/state-exempt.tsv` names every other one. Between them they
//! cover each `impl State` in non-test workspace source — a state cannot
//! exist without a row or an exemption, and a listing without an impl
//! fails too. Each state's crate owns the rows whose first source lives
//! under its `src/` and checks them in its parity test; the coverage scan
//! runs workspace-wide from here, so crates without a parity test of their
//! own are covered as well.
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

/// A declared `impl State`: the crate and file it lives in and whether it
/// sets `CODE_VERSIONED`.
struct Declared {
    krate: String,
    path: PathBuf,
    code_versioned: bool,
}

/// Whether a `cfg` predicate can only hold in test builds: `test` itself
/// or an `all(...)` that requires it. `any(...)` and `not(...)` can hold
/// in production, so their modules stay part of the scan.
fn test_only(predicate: &syn::Meta) -> bool {
    match predicate {
        syn::Meta::Path(path) => path.is_ident("test"),
        syn::Meta::List(list) if list.path.is_ident("all") => list
            .parse_args_with(
                syn::punctuated::Punctuated::<syn::Meta, syn::Token![,]>::parse_terminated,
            )
            .is_ok_and(|args| args.iter().any(test_only)),
        _ => false,
    }
}

/// Whether `attrs` hold a `#[cfg(..)]` that only test builds satisfy.
fn test_gated(attrs: &[syn::Attribute]) -> bool {
    attrs.iter().any(|attr| {
        attr.path().is_ident("cfg")
            && attr
                .parse_args::<syn::Meta>()
                .is_ok_and(|predicate| test_only(&predicate))
    })
}

/// The value of a `#[path = ".."]` among `attrs`.
fn path_attr(attrs: &[syn::Attribute]) -> Option<String> {
    attrs.iter().find_map(|attr| match &attr.meta {
        syn::Meta::NameValue(syn::MetaNameValue {
            path,
            value:
                syn::Expr::Lit(syn::ExprLit {
                    lit: syn::Lit::Str(value),
                    ..
                }),
            ..
        }) if path.is_ident("path") => Some(value.value()),
        _ => None,
    })
}

/// Whether `tokens`, at any depth, hold `const` followed by `NAMESPACE`:
/// a `State` impl or its items written inside a macro, which the syntax
/// tree cannot see into. A bare `T::NAMESPACE` is not a declaration.
fn declares_namespace(tokens: proc_macro2::TokenStream) -> bool {
    let mut after_const = false;
    tokens.into_iter().any(|token| match token {
        proc_macro2::TokenTree::Group(group) => {
            after_const = false;
            declares_namespace(group.stream())
        }
        proc_macro2::TokenTree::Ident(ident) => {
            let found = after_const && ident == "NAMESPACE";
            after_const = ident == "const";
            found
        }
        _ => {
            after_const = false;
            false
        }
    })
}

/// What one source file declares: the files its `#[cfg(test)] mod name;`
/// modules can resolve to, the `(namespace, code_versioned)` of each
/// `impl State` outside `#[cfg(test)]` modules, at any depth (inside
/// functions too), and the declarations it cannot read: a non-literal
/// `NAMESPACE` or one written inside a macro. Those fail the scan unless the
/// whole file is test code.
struct Scan<'a> {
    path: &'a Path,
    /// The inline modules enclosing the item being visited, as directory
    /// components: each module's name, or its `#[path]`.
    modules: Vec<String>,
    test_files: Vec<PathBuf>,
    impls: Vec<(String, bool)>,
    unreadable: Vec<String>,
}

/// The value of the `const {name}` among `items`.
fn constant<'a>(items: &'a [syn::ImplItem], name: &str) -> Option<&'a syn::Expr> {
    items.iter().find_map(|item| match item {
        syn::ImplItem::Const(constant) if constant.ident == name => Some(&constant.expr),
        _ => None,
    })
}

impl Scan<'_> {
    /// The files `mod {module};` here can resolve to, by the reference's
    /// rules: relative to this file's directory, under a directory named
    /// after the file unless it is a `lib.rs`, `main.rs` or `mod.rs`, then
    /// through the enclosing inline modules. A `#[path]` outside any inline
    /// module is relative to this file's directory alone.
    fn module_files(&self, module: &syn::ItemMod) -> Vec<PathBuf> {
        let mut base = self
            .path
            .parent()
            .expect("a path inside the workspace")
            .to_path_buf();
        let stem = self
            .path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .expect("a UTF-8 file name");
        let nested = |mut base: PathBuf| {
            if !matches!(stem, "lib" | "main" | "mod") {
                base.push(stem);
            }
            base.extend(&self.modules);
            base
        };
        if let Some(path) = path_attr(&module.attrs) {
            if !self.modules.is_empty() {
                base = nested(base);
            }
            return vec![base.join(path)];
        }
        let base = nested(base);
        let name = module.ident.to_string();
        vec![
            base.join(format!("{name}.rs")),
            base.join(&name).join("mod.rs"),
        ]
    }
}

impl<'ast> syn::visit::Visit<'ast> for Scan<'_> {
    fn visit_item_mod(&mut self, module: &'ast syn::ItemMod) {
        if test_gated(&module.attrs) {
            if module.content.is_none() {
                let files = self.module_files(module);
                self.test_files.extend(files);
            }
            return;
        }
        self.modules
            .push(path_attr(&module.attrs).unwrap_or_else(|| module.ident.to_string()));
        syn::visit::visit_item_mod(self, module);
        self.modules.pop();
    }

    /// A trait impl counts by what it declares, not how it names the
    /// trait: the one trait in the workspace with a `NAMESPACE` const is
    /// `State`, so a renamed import, a qualified path or generics are all
    /// seen.
    fn visit_item_impl(&mut self, block: &'ast syn::ItemImpl) {
        if block.trait_.is_some()
            && let Some(namespace) = constant(&block.items, "NAMESPACE")
        {
            if let syn::Expr::Lit(syn::ExprLit {
                lit: syn::Lit::Str(namespace),
                ..
            }) = namespace
            {
                let code_versioned = matches!(
                    constant(&block.items, "CODE_VERSIONED"),
                    Some(syn::Expr::Lit(syn::ExprLit {
                        lit: syn::Lit::Bool(syn::LitBool { value: true, .. }),
                        ..
                    }))
                );
                self.impls.push((namespace.value(), code_versioned));
            } else {
                self.unreadable
                    .push("impl State without a NAMESPACE literal".into());
            }
        }
        syn::visit::visit_item_impl(self, block);
    }

    /// Macro bodies are opaque to the syntax tree: `macro_rules!`
    /// definitions, item and expression invocations and macros among an
    /// impl's items all land here, so a `State` declared through one fails
    /// closed instead of slipping past the guard.
    fn visit_macro(&mut self, mac: &'ast syn::Macro) {
        if declares_namespace(mac.tokens.clone()) {
            let name = mac
                .path
                .segments
                .last()
                .map(|segment| segment.ident.to_string())
                .unwrap_or_default();
            self.unreadable.push(format!(
                "`{name}!` declares a NAMESPACE the scan cannot read; write the impl State \
                 outside the macro"
            ));
        }
        syn::visit::visit_macro(self, mac);
    }
}

fn scan(path: &Path) -> Scan<'_> {
    let text =
        std::fs::read_to_string(path).unwrap_or_else(|error| panic!("{}: {error}", path.display()));
    scan_text(path, &text)
}

fn scan_text<'a>(path: &'a Path, text: &str) -> Scan<'a> {
    let file = syn::parse_file(text).unwrap_or_else(|error| panic!("{}: {error}", path.display()));
    let mut scan = Scan {
        path,
        modules: Vec::new(),
        test_files: Vec::new(),
        impls: Vec::new(),
        unreadable: Vec::new(),
    };
    syn::visit::Visit::visit_file(&mut scan, &file);
    scan
}

/// Every `impl State` in non-test source across the workspace, keyed by
/// `NAMESPACE`: `crates/*/src/**/*.rs`, minus `#[cfg(test)]` modules —
/// inline ones and `mod name;` file modules — and anything under a
/// `tests` directory.
fn declared() -> BTreeMap<String, Declared> {
    let root = crate::versions::root();
    let mut declared = BTreeMap::new();
    for entry in std::fs::read_dir(root.join("crates")).unwrap().flatten() {
        let src = entry.path().join("src");
        if !src.is_dir() {
            continue;
        }
        let krate = entry.file_name().to_string_lossy().into_owned();
        let files: Vec<PathBuf> = crate::versions::files_under(&src)
            .into_iter()
            .filter(|path| path.extension().is_some_and(|e| e == "rs"))
            .collect();
        let scans: Vec<Scan> = files.iter().map(|file| scan(file)).collect();
        // A `#[cfg(test)] mod name;` resolves to a file; skip those files
        // the way the inline modules' contents are skipped.
        let skip: BTreeSet<PathBuf> = scans
            .iter()
            .flat_map(|scan| scan.test_files.iter().cloned())
            .collect();
        for scan in scans {
            let file = scan.path;
            if skip.contains(file) || file.components().any(|part| part.as_os_str() == "tests") {
                continue;
            }
            if let Some(problem) = scan.unreadable.first() {
                panic!("{}: {problem}", file.display());
            }
            for (namespace, code_versioned) in scan.impls {
                let previous = declared.insert(
                    namespace.clone(),
                    Declared {
                        krate: krate.clone(),
                        path: file.to_owned(),
                        code_versioned,
                    },
                );
                if let Some(previous) = previous {
                    panic!(
                        "{namespace}: declared in {} and {}",
                        previous.path.display(),
                        file.display()
                    );
                }
            }
        }
    }
    declared
}

/// `tests/state-exempt.tsv`: the namespace of every `impl State` that is
/// not code-versioned and the reason it needs no row.
fn exemptions() -> BTreeMap<String, String> {
    let path = crate::versions::root().join("tests/state-exempt.tsv");
    let table = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("{}: {error}", path.display()));
    table
        .lines()
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(|line| {
            let mut fields = line.split('\t');
            let (Some(name), Some(reason), None) = (fields.next(), fields.next(), fields.next())
            else {
                panic!("tests/state-exempt.tsv: malformed row {line:?}");
            };
            (name.to_owned(), reason.to_owned())
        })
        .fold(BTreeMap::new(), |mut table, (name, reason)| {
            if table.insert(name.clone(), reason).is_some() {
                panic!("tests/state-exempt.tsv: {name} is listed twice");
            }
            table
        })
}

/// The workspace-wide half of the guard: a code-versioned impl is a row
/// in `tests/state-versions.tsv`, any other is an entry in
/// `tests/state-exempt.tsv`, and a listing whose impl is gone or whose
/// flag disagrees with the table fails.
fn coverage(declared: &BTreeMap<String, Declared>) {
    let rows: BTreeSet<String> = crate::versions::table("state-versions.tsv")
        .iter()
        .map(|row| row.name.clone())
        .collect();
    let exempt = exemptions();
    for (name, state) in declared {
        let path = state.path.display();
        if state.code_versioned {
            assert!(
                rows.contains(name),
                "{name}: CODE_VERSIONED impl in {path} has no row in \
                 tests/state-versions.tsv — add one"
            );
            assert!(
                !exempt.contains_key(name),
                "{name}: CODE_VERSIONED impl in {path} is listed in \
                 tests/state-exempt.tsv — remove it"
            );
        } else {
            assert!(
                exempt.contains_key(name),
                "{name}: impl in {path} is not CODE_VERSIONED — set the \
                 flag and add a row, or list it in tests/state-exempt.tsv"
            );
            assert!(
                !rows.contains(name),
                "{name}: impl in {path} is not CODE_VERSIONED but has a \
                 row in tests/state-versions.tsv — remove it or set the flag"
            );
        }
    }
    for name in exempt.keys() {
        assert!(
            declared.contains_key(name),
            "{name}: listed in tests/state-exempt.tsv but no impl State \
             declares it — remove the entry"
        );
    }
    for name in &rows {
        let Some(state) = declared.get(name) else {
            panic!(
                "{name}: a row in tests/state-versions.tsv but no impl \
                    State declares it — remove the row or restore the impl"
            )
        };
        assert!(
            state.code_versioned,
            "{name}: a row in tests/state-versions.tsv but the impl in {} \
             is not CODE_VERSIONED — move it to tests/state-exempt.tsv",
            state.path.display()
        );
    }
}

/// Every registered state has a row whose version and source hash still
/// hold, every code-versioned state the crate's sources declare is
/// registered, no row is stale, and every `impl State` in the workspace
/// is a row or an exemption. On a source change the failure names the
/// new hash; bump the state's `VERSION` and update its row in the same
/// commit. A listed crate must be in Cargo.lock; its locked version keys
/// the cache entry, not the row.
pub fn check(krate: &str, registered: &std::collections::BTreeMap<&'static str, u32>) {
    let declared = declared();
    let mine = declared
        .iter()
        .filter(|(_, state)| state.krate == krate && state.code_versioned)
        .map(|(name, _)| name.clone())
        .collect();
    crate::versions::check("state-versions.tsv", krate, registered, mine);
    coverage(&declared);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_files(path: &str, text: &str) -> Vec<PathBuf> {
        scan_text(Path::new(path), text).test_files
    }

    /// A test module's file resolves through the inline modules around it
    /// and through `#[path]`, so neither skips the production `bar.rs`.
    #[test]
    fn test_module_files_follow_inline_modules_and_path() {
        assert_eq!(
            test_files("src/lib.rs", "mod inner { #[cfg(test)] mod bar; }"),
            [
                PathBuf::from("src/inner/bar.rs"),
                PathBuf::from("src/inner/bar/mod.rs")
            ]
        );
        assert_eq!(
            test_files(
                "src/foo.rs",
                "mod inner { #[cfg(all(test, unix))] mod bar; }"
            ),
            [
                PathBuf::from("src/foo/inner/bar.rs"),
                PathBuf::from("src/foo/inner/bar/mod.rs")
            ]
        );
        assert_eq!(
            test_files(
                "src/lib.rs",
                "mod bar;\n#[cfg(test)]\n#[path = \"bar_test.rs\"]\nmod bar_tests;"
            ),
            [PathBuf::from("src/bar_test.rs")]
        );
        assert_eq!(
            test_files(
                "src/foo.rs",
                "#[path = \"x\"] mod inner { #[cfg(test)] #[path = \"t.rs\"] mod t; }"
            ),
            [PathBuf::from("src/foo/x/t.rs")]
        );
        assert_eq!(
            test_files("src/foo.rs", "#[cfg(test)] mod tests;"),
            [
                PathBuf::from("src/foo/tests.rs"),
                PathBuf::from("src/foo/tests/mod.rs")
            ]
        );
    }

    /// A `State` declared through a macro cannot be read from the syntax
    /// tree, so it is reported; a `T::NAMESPACE` read in a macro is not.
    #[test]
    fn a_namespace_inside_a_macro_is_unreadable() {
        let scan = |text| scan_text(Path::new("src/lib.rs"), text).unreadable.len();
        assert_eq!(
            scan(
                "macro_rules! state { ($name:ident) => { impl State for $name { \
                 const NAMESPACE: &'static str = \"x\"; const VERSION: u32 = 1; } }; }"
            ),
            1
        );
        assert_eq!(
            scan("impl State for X { declare! { const NAMESPACE: &'static str = \"x\"; } }"),
            1
        );
        assert_eq!(
            scan("fn f() { m!(impl State for X { const NAMESPACE: &str = \"x\"; }); }"),
            1
        );
        assert_eq!(
            scan("fn f<T: State>() { tracing::debug!(\"{}\", T::NAMESPACE); }"),
            0
        );
        assert_eq!(
            scan("#[cfg(test)] mod tests { m! { const NAMESPACE: &str = \"x\"; } }"),
            0
        );
        assert_eq!(
            scan("impl State for X { const NAMESPACE: &'static str = NAME; }"),
            1
        );
    }
}
