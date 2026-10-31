//! Maven version order, implemented from the POM reference's "Version Order
//! Specification" (https://maven.apache.org/pom.html#Version_Order_Specification).
use super::*;
use std::cmp::Ordering;

#[derive(Clone, Debug)]
enum Item {
    Number(String),
    Qualifier(String),
    /// A hyphen (or digit/letter transition) starts a nested sequence.
    List(Vec<Item>),
}

/// Rank of a qualifier; unknown qualifiers sort after `sp`, lexically.
fn rank(qualifier: &str) -> (u8, &str) {
    match qualifier {
        "alpha" => (0, ""),
        "beta" => (1, ""),
        "milestone" => (2, ""),
        "rc" | "cr" => (3, ""),
        "snapshot" => (4, ""),
        "" | "final" | "ga" | "release" => (5, ""),
        "sp" => (6, ""),
        other => (7, other),
    }
}
fn is_null(item: &Item) -> bool {
    match item {
        Item::Number(n) => n.bytes().all(|b| b == b'0'),
        Item::Qualifier(q) => rank(q).0 == 5,
        Item::List(items) => items.is_empty(),
    }
}
fn number(a: &str, b: &str) -> Ordering {
    let (a, b) = (a.trim_start_matches('0'), b.trim_start_matches('0'));
    a.len().cmp(&b.len()).then_with(|| a.cmp(b))
}
fn qualifier(a: &str, b: &str) -> Ordering {
    rank(a).cmp(&rank(b))
}
/// Compare an item with the padding the other side would supply.
fn against_null(item: &Item) -> Ordering {
    match item {
        Item::Number(n) => number(n, "0"),
        Item::Qualifier(q) => qualifier(q, ""),
        Item::List(items) => items.first().map_or(Ordering::Equal, against_null),
    }
}
fn compare(a: &Item, b: &Item) -> Ordering {
    use Item::*;
    match (a, b) {
        (Number(a), Number(b)) => number(a, b),
        (Number(_), _) => Ordering::Greater,
        (Qualifier(_), Number(_)) => Ordering::Less,
        (Qualifier(a), Qualifier(b)) => qualifier(a, b),
        (Qualifier(_), List(_)) => Ordering::Less,
        (List(_), Number(_)) => Ordering::Less,
        (List(_), Qualifier(_)) => Ordering::Greater,
        (List(a), List(b)) => {
            for i in 0..a.len().max(b.len()) {
                let order = match (a.get(i), b.get(i)) {
                    (Some(x), Some(y)) => compare(x, y),
                    (Some(x), None) => against_null(x),
                    (None, Some(y)) => against_null(y).reverse(),
                    (None, None) => Ordering::Equal,
                };
                if order != Ordering::Equal {
                    return order;
                }
            }
            Ordering::Equal
        }
    }
}
fn trim(items: &mut Vec<Item>) {
    while items.last().is_some_and(is_null) {
        items.pop();
    }
}

/// Tokenise and normalise one version into nested sequences.
fn items(version: &str) -> Item {
    let text = version.to_lowercase();
    let chars: Vec<char> = text.chars().collect();
    // Stack of open sequences; a hyphen or transition opens a nested one.
    let mut stack: Vec<Vec<Item>> = vec![Vec::new()];
    let mut token = String::new();
    let mut digits = false;
    let push =
        |stack: &mut Vec<Vec<Item>>, token: &mut String, digits: bool, next: Option<char>| {
            let item = if token.is_empty() {
                Item::Number("0".into())
            } else if digits {
                Item::Number(std::mem::take(token))
            } else {
                let mut q = std::mem::take(token);
                // a, b and m abbreviate alpha, beta and milestone before a number.
                if next.is_some_and(|c| c.is_ascii_digit()) {
                    q = match q.as_str() {
                        "a" => "alpha".into(),
                        "b" => "beta".into(),
                        "m" => "milestone".into(),
                        _ => q,
                    };
                }
                Item::Qualifier(q)
            };
            stack.last_mut().expect("open sequence").push(item);
        };
    let open = |stack: &mut Vec<Vec<Item>>| {
        if let Some(last) = stack.last_mut() {
            trim(last);
        }
        stack.push(Vec::new());
    };
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '.' {
            push(&mut stack, &mut token, digits, None);
            digits = false;
        } else if c == '-' || c == '_' {
            push(&mut stack, &mut token, digits, None);
            digits = false;
            open(&mut stack);
        } else if c.is_ascii_digit() {
            if !digits && !token.is_empty() {
                push(&mut stack, &mut token, false, Some(c));
                open(&mut stack);
            }
            digits = true;
            token.push(c);
        } else {
            if digits && !token.is_empty() {
                push(&mut stack, &mut token, true, None);
                open(&mut stack);
            } else if token.is_empty()
                && i > 0
                && chars[i - 1] == '.'
                && !stack.last().is_some_and(Vec::is_empty)
            {
                // `.qualifier` sorts like `-qualifier`.
                open(&mut stack);
            }
            digits = false;
            token.push(c);
        }
        i += 1;
    }
    push(&mut stack, &mut token, digits, None);
    // Close nested sequences from the innermost outwards.
    while stack.len() > 1 {
        let mut inner = stack.pop().expect("nested sequence");
        trim(&mut inner);
        stack
            .last_mut()
            .expect("outer sequence")
            .push(Item::List(inner));
    }
    let mut root = stack.pop().expect("root sequence");
    trim(&mut root);
    Item::List(root)
}

pub(super) fn order(a: &str, b: &str) -> Ordering {
    compare(&items(a), &items(b))
}

/// Leading numbers as semver for update classification; pre-release
/// qualifiers and common early-access words mark a prerelease.
pub(super) fn parse(value: &str) -> Result<Parsed> {
    let value = value.trim();
    if value.is_empty() || !value.starts_with(|c: char| c.is_ascii_digit()) {
        return Err(Error::Invalid(value.into()));
    }
    let numbers: Vec<u64> = value
        .split(|c: char| !c.is_ascii_digit())
        .take_while(|p| !p.is_empty())
        .take(3)
        .map(|p| p.parse().map_err(|_| Error::Invalid(value.into())))
        .collect::<Result<_>>()?;
    let mut version = Version::new(
        numbers[0],
        *numbers.get(1).unwrap_or(&0),
        *numbers.get(2).unwrap_or(&0),
    );
    let lower = value.to_lowercase();
    let pre = lower
        .split(|c: char| !c.is_ascii_alphabetic())
        .filter(|w| !w.is_empty())
        .any(|w| {
            matches!(
                w,
                "alpha"
                    | "a"
                    | "beta"
                    | "b"
                    | "milestone"
                    | "m"
                    | "rc"
                    | "cr"
                    | "snapshot"
                    | "ea"
                    | "preview"
                    | "dev"
                    | "pre"
            )
        });
    if pre {
        version.pre = semver::Prerelease::new("pre").expect("static prerelease");
    }
    Ok(Parsed(version, None, Some(value.into())))
}

/// The release variant a version belongs to (Guava's `jre`/`android`,
/// Hibernate's `Final`): its last qualifier that is not a pre-release word.
pub fn variant(version: &str) -> Option<String> {
    version
        .to_lowercase()
        .split(|c: char| !c.is_ascii_alphabetic())
        .rev()
        .find(|w| {
            !w.is_empty()
                && !matches!(
                    *w,
                    "alpha"
                        | "a"
                        | "beta"
                        | "b"
                        | "milestone"
                        | "m"
                        | "rc"
                        | "cr"
                        | "snapshot"
                        | "ea"
                        | "preview"
                        | "dev"
                        | "pre"
                )
        })
        .map(str::to_owned)
}

/// Bare versions are exact; `[a,b)`-style ranges may be joined by commas.
pub(super) fn matches(range: &str, version: &Parsed) -> Result<bool> {
    let current = version.2.as_deref().expect("Maven parsed version");
    let range = range.trim();
    if !range.starts_with(['[', '(']) {
        return Ok(order(range, current).is_eq());
    }
    intervals(range, current, order)
}

/// A single interval extended to include `target`: its upper bound becomes the
/// target, inclusive, as Cargo's convex interval does. A set of intervals has
/// no one bound to move.
pub(super) fn widen(range: &str, target: &str) -> Option<String> {
    let range = range.trim();
    let body = range.strip_prefix(['[', '('])?.strip_suffix([']', ')'])?;
    if body.contains(['[', '(', ']', ')']) {
        return None;
    }
    let low = match body.split_once(',') {
        Some((low, _)) => low.trim(),
        None => body.trim(),
    };
    let open = &range[..1];
    Some(format!("{open}{low},{target}]"))
}

/// Interval sets such as `[1.0,2.0)` or `(,1.0],[1.2,)`, shared with NuGet.
pub(super) fn intervals(
    range: &str,
    current: &str,
    order: fn(&str, &str) -> Ordering,
) -> Result<bool> {
    let mut rest = range;
    while !rest.is_empty() {
        let close = rest
            .find([']', ')'])
            .ok_or_else(|| Error::Invalid(range.into()))?;
        let (set, tail) = rest.split_at(close + 1);
        let inclusive_low = set.starts_with('[');
        let inclusive_high = set.ends_with(']');
        let body = &set[1..set.len() - 1];
        let ok = match body.split_once(',') {
            None => order(body.trim(), current).is_eq(),
            Some((low, high)) => {
                let (low, high) = (low.trim(), high.trim());
                (low.is_empty()
                    || match order(current, low) {
                        Ordering::Greater => true,
                        Ordering::Equal => inclusive_low,
                        Ordering::Less => false,
                    })
                    && (high.is_empty()
                        || match order(current, high) {
                            Ordering::Less => true,
                            Ordering::Equal => inclusive_high,
                            Ordering::Greater => false,
                        })
            }
        };
        if ok {
            return Ok(true);
        }
        rest = tail.trim_start_matches([',', ' ']);
    }
    Ok(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn specification_end_result_examples() {
        use Ordering::*;
        for (a, b, expected) in [
            ("1", "1.1", Less),
            ("1-snapshot", "1", Less),
            ("1", "1-sp", Less),
            ("1-foo2", "1-foo10", Less),
            ("1.foo", "1-foo", Equal),
            ("1-foo", "1-1", Less),
            ("1-1", "1.1", Less),
            ("1.ga", "1-ga", Equal),
            ("1-ga", "1-0", Equal),
            ("1-0", "1_0", Equal),
            ("1_0", "1.0", Equal),
            ("1.0", "1", Equal),
            ("1-sp", "1-ga", Greater),
            ("1-sp.1", "1-ga.1", Greater),
            ("1-sp-1", "1-ga-1", Less),
            ("1-a1", "1-alpha-1", Equal),
            ("1.0-alpha1", "1.0-ALPHA1", Equal),
            ("1.7", "1.K", Greater),
            ("5.zebra", "5.aardvark", Greater),
            ("2.0-rc1", "2.0", Less),
            ("33.7.2-jre", "33.4.8-jre", Greater),
            ("1.0.0-M1", "1.0.0-RC1", Less),
            ("1.10", "1.9", Greater),
        ] {
            assert_eq!(order(a, b), expected, "{a} vs {b}");
            assert_eq!(order(b, a), expected.reverse(), "{b} vs {a}");
        }
    }
    #[test]
    fn ranges_and_prereleases() {
        let v = |s| parse(s).unwrap();
        assert!(matches("[1.0,2.0)", &v("1.5")).unwrap());
        assert!(!matches("[1.0,2.0)", &v("2.0")).unwrap());
        assert!(matches("(,1.0],[1.2,)", &v("1.3")).unwrap());
        assert!(!matches("(,1.0],[1.2,)", &v("1.1")).unwrap());
        assert!(matches("1.0", &v("1.0.0")).unwrap());
        assert!(!v("2.0-rc1").0.pre.is_empty());
        assert!(!v("1.0.0-M2").0.pre.is_empty());
        assert!(v("33.7.2-jre").0.pre.is_empty());
        assert_eq!(v("33.7.2-jre").0, Version::new(33, 7, 2));
        assert_eq!(variant("33.7.2-jre").as_deref(), Some("jre"));
        assert_eq!(variant("6.4.4.Final").as_deref(), Some("final"));
        assert_eq!(variant("2.0-rc1"), None);
    }
}
