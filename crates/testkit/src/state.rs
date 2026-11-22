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

/// The files a `#[cfg(test)] mod name;` in `file` can resolve to.
fn module_files(file: &Path, name: &str) -> [PathBuf; 2] {
    let parent = file.parent().expect("a path inside the workspace");
    let stem = file
        .file_stem()
        .and_then(|stem| stem.to_str())
        .expect("a UTF-8 file name");
    let base = if stem == "lib" || stem == "main" || stem == "mod" {
        parent.to_path_buf()
    } else {
        parent.join(stem)
    };
    [
        base.join(format!("{name}.rs")),
        base.join(name).join("mod.rs"),
    ]
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

/// What one source file declares: the names of its `#[cfg(test)] mod name;`
/// file modules and the `(namespace, code_versioned)` of each `impl State`
/// outside `#[cfg(test)]` modules, at any depth (inside functions too).
struct Scan<'a> {
    path: &'a Path,
    test_files: Vec<String>,
    impls: Vec<(String, bool)>,
}

/// The value of the `const {name}` among `items`.
fn constant<'a>(items: &'a [syn::ImplItem], name: &str) -> Option<&'a syn::Expr> {
    items.iter().find_map(|item| match item {
        syn::ImplItem::Const(constant) if constant.ident == name => Some(&constant.expr),
        _ => None,
    })
}

impl<'ast> syn::visit::Visit<'ast> for Scan<'_> {
    fn visit_item_mod(&mut self, module: &'ast syn::ItemMod) {
        if test_gated(&module.attrs) {
            if module.content.is_none() {
                self.test_files.push(module.ident.to_string());
            }
            return;
        }
        syn::visit::visit_item_mod(self, module);
    }

    /// A trait impl counts by what it declares, not how it names the
    /// trait: the one trait in the workspace with a `NAMESPACE` const is
    /// `State`, so a renamed import, a qualified path or generics are all
    /// seen.
    fn visit_item_impl(&mut self, block: &'ast syn::ItemImpl) {
        if block.trait_.is_some()
            && let Some(namespace) = constant(&block.items, "NAMESPACE")
        {
            let syn::Expr::Lit(syn::ExprLit {
                lit: syn::Lit::Str(namespace),
                ..
            }) = namespace
            else {
                panic!(
                    "{}: impl State without a NAMESPACE literal",
                    self.path.display()
                )
            };
            let code_versioned = matches!(
                constant(&block.items, "CODE_VERSIONED"),
                Some(syn::Expr::Lit(syn::ExprLit {
                    lit: syn::Lit::Bool(syn::LitBool { value: true, .. }),
                    ..
                }))
            );
            self.impls.push((namespace.value(), code_versioned));
        }
        syn::visit::visit_item_impl(self, block);
    }
}

fn scan(path: &Path) -> Scan<'_> {
    let text =
        std::fs::read_to_string(path).unwrap_or_else(|error| panic!("{}: {error}", path.display()));
    let file = syn::parse_file(&text).unwrap_or_else(|error| panic!("{}: {error}", path.display()));
    let mut scan = Scan {
        path,
        test_files: Vec::new(),
        impls: Vec::new(),
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
            .flat_map(|scan| {
                scan.test_files
                    .iter()
                    .flat_map(|name| module_files(scan.path, name))
            })
            .collect();
        for scan in scans {
            let file = scan.path;
            if skip.contains(file) || file.components().any(|part| part.as_os_str() == "tests") {
                continue;
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
