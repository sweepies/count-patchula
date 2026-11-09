use patchula_config::Registry;
use std::cmp::Ordering;

#[test]
fn registries_order_by_their_record_and_never_tie_when_unequal() {
    let url = Registry::Url("same".into());
    let unusable = Registry::Unusable("same".into());
    assert_ne!(url, unusable);
    assert_ne!(url.cmp(&unusable), Ordering::Equal);
    // The recorded text still decides first.
    assert!(Registry::Unusable("a".into()) < Registry::Url("b".into()));
    let set: std::collections::BTreeSet<_> = [url, unusable].into_iter().collect();
    assert_eq!(set.len(), 2);
}
