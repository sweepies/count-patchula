use patchula_config::Filter;
use patchula_versioning::Ecosystem;

#[test]
fn an_invalid_semver_range_is_an_error_even_when_the_value_is_not_a_version() {
    let filter = "semver:>>>1".parse::<Filter>().unwrap();
    // "^1.2" is a range, not a version: it never matches, but the bad
    // requirement still surfaces.
    assert!(filter.matches(Ecosystem::Npm, "^1.2").is_err());
    let good = "semver:>=1".parse::<Filter>().unwrap();
    assert_eq!(good.matches(Ecosystem::Npm, "^1.2"), Ok(false));
    assert_eq!(good.matches(Ecosystem::Npm, "1.4.0"), Ok(true));
}
