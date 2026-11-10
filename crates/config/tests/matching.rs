use patchula_config::Patterns;

#[test]
fn empty_and_blank_patterns_are_refused() {
    assert_eq!(
        Patterns::compile(&["".into()]).err().unwrap(),
        "pattern must not be empty"
    );
    assert_eq!(
        Patterns::compile(&["!".into()]).err().unwrap(),
        "pattern must not be empty"
    );
    assert_eq!(
        Patterns::compile(&["glob:".into()]).err().unwrap(),
        "glob must not be empty"
    );
    assert!(Patterns::compile(&["regex:(".into()]).is_err());
}

#[test]
fn globs_regexes_and_negations_select_packages() {
    let patterns = Patterns::compile(&[
        "npm @scope/*".into(),
        "!npm @scope/internal".into(),
        "regex:^cargo:".into(),
        "pypi:requests".into(),
    ])
    .unwrap();
    assert!(patterns.matches("npm @scope/ui"));
    // An exclusion wins over a positive match on either name.
    assert!(!patterns.matches("npm @scope/internal"));
    assert!(patterns.matches("cargo:serde"));
    assert!(patterns.matches("pypi:requests"));
    assert!(!patterns.matches("gem:rails"));
    // `matches_any` lets a negation on either alias win too.
    assert!(patterns.matches_any("cargo:serde", "serde"));
    assert!(!patterns.matches_any("internal", "npm @scope/internal"));
    // No positive patterns means any package matches.
    let open = Patterns::compile(&["!npm left-pad".into()]).unwrap();
    assert!(open.matches("anything"));
    assert!(!open.matches("npm left-pad"));
}

#[test]
fn a_filter_that_is_neither_regex_nor_semver_is_refused() {
    assert_eq!(
        patchula_config::Filter::compile("^1.2").err().unwrap(),
        "expected regex:... or semver:... version filter"
    );
    assert!(patchula_config::Filter::compile("!").is_err());
}
