//! Composer versions and constraints. Versions have up to four numbers and an
//! optional stability (`alpha` < `beta` < `RC` < stable < `patch`).
//! Constraints combine `^`, `~` (`~1.2` allows `<2.0`, `~1.2.3` allows
//! `<1.3`), comparisons, `1.2.*` wildcards and `1.0 - 2.0` hyphen ranges,
//! joined by `,`/spaces (and) and `||` (or).
use super::*;
use std::cmp::Ordering;

/// Release numbers, stability rank and stability number.
fn key(version: &str) -> Option<([u64; 4], u8, u64)> {
    let version = version.trim();
    let version = version.strip_prefix(['v', 'V']).unwrap_or(version);
    let end = version
        .find(|c: char| !(c.is_ascii_digit() || c == '.'))
        .unwrap_or(version.len());
    let (release, rest) = version.split_at(end);
    let release = release.trim_end_matches('.');
    let parts: Vec<u64> = release
        .split('.')
        .map(|p| p.parse().ok())
        .collect::<Option<_>>()?;
    if parts.is_empty() || parts.len() > 4 {
        return None;
    }
    let mut numbers = [0; 4];
    numbers[..parts.len()].copy_from_slice(&parts);
    let rest = rest
        .trim_start_matches(['-', '_', '.'])
        .to_ascii_lowercase();
    let word_end = rest
        .find(|c: char| !c.is_ascii_alphabetic())
        .unwrap_or(rest.len());
    let (word, number) = rest.split_at(word_end);
    let rank = match word {
        "" | "stable" => 4,
        "alpha" | "a" => 1,
        "beta" | "b" => 2,
        "rc" => 3,
        "patch" | "pl" | "p" => 5,
        _ => return None,
    };
    let number = number.trim_start_matches(['.', '-']);
    let number = if number.is_empty() {
        0
    } else {
        number.parse().ok()?
    };
    Some((numbers, rank, number))
}

pub(super) fn order(a: &str, b: &str) -> Ordering {
    match (key(a), key(b)) {
        (Some(a), Some(b)) => a.cmp(&b),
        _ => a.cmp(b),
    }
}

pub(super) fn parse(value: &str) -> Result<Parsed> {
    let (numbers, rank, _) = key(value).ok_or_else(|| Error::Invalid(value.into()))?;
    let mut version = Version::new(numbers[0], numbers[1], numbers[2]);
    if rank < 4 {
        version.pre = semver::Prerelease::new("pre").expect("static prerelease");
    }
    Ok(Parsed(version, None, Some(value.trim().into())))
}

fn precision(version: &str) -> usize {
    let version = version.trim().trim_start_matches(['v', 'V']);
    version
        .split(|c: char| !(c.is_ascii_digit() || c == '.'))
        .next()
        .unwrap_or("")
        .trim_end_matches('.')
        .split('.')
        .count()
}
/// The exclusive upper bound that bumps release number `index`.
fn bump(version: &str, index: usize) -> Option<String> {
    let (numbers, _, _) = key(version)?;
    let mut upper = numbers;
    upper[index] += 1;
    for n in upper.iter_mut().skip(index + 1) {
        *n = 0;
    }
    Some(
        upper
            .iter()
            .map(u64::to_string)
            .collect::<Vec<_>>()
            .join("."),
    )
}

fn satisfies(clause: &str, current: &str) -> Result<bool> {
    let clause = clause.trim();
    let clause = clause.split('@').next().unwrap_or(clause).trim();
    let invalid = || Error::Invalid(clause.into());
    if clause == "*" || clause.is_empty() {
        return Ok(true);
    }
    let at_least = |base: &str| order(current, base) != Ordering::Less;
    let below = |upper: &str| order(current, upper) == Ordering::Less;
    if let Some(base) = clause.strip_prefix('^') {
        let (numbers, _, _) = key(base).ok_or_else(invalid)?;
        let digits = precision(base);
        let index = numbers[..digits]
            .iter()
            .position(|n| *n != 0)
            .unwrap_or(digits - 1);
        return Ok(at_least(base) && below(&bump(base, index).ok_or_else(invalid)?));
    }
    if let Some(base) = clause.strip_prefix('~') {
        let digits = precision(base);
        let index = if digits == 1 { 0 } else { digits - 2 };
        return Ok(at_least(base) && below(&bump(base, index).ok_or_else(invalid)?));
    }
    if let Some(prefix) = clause.strip_suffix(".*") {
        let digits = precision(prefix);
        let base = format!("{prefix}.0");
        return Ok(at_least(&base) && below(&bump(prefix, digits - 1).ok_or_else(invalid)?));
    }
    // Longest operators first, so `>=` is not read as `>`.
    for op in [">=", "<=", "!=", "==", ">", "<", "="] {
        if let Some(base) = clause.strip_prefix(op) {
            let base = base.trim();
            key(base).ok_or_else(invalid)?;
            let o = order(current, base);
            return Ok(match op {
                ">=" => o != Ordering::Less,
                "<=" => o != Ordering::Greater,
                "!=" => o != Ordering::Equal,
                ">" => o == Ordering::Greater,
                "<" => o == Ordering::Less,
                _ => o == Ordering::Equal,
            });
        }
    }
    key(clause).ok_or_else(invalid)?;
    Ok(order(current, clause) == Ordering::Equal)
}

pub(super) fn matches(constraint: &str, version: &Parsed) -> Result<bool> {
    let current = version.2.as_deref().expect("Composer parsed version");
    for alternative in constraint.split("||").flat_map(|a| a.split('|')) {
        let alternative = alternative.trim();
        // `1.0 - 2.0`: inclusive lower bound, upper bound up to its precision.
        if let Some((low, high)) = alternative.split_once(" - ") {
            let upper = bump(high, precision(high) - 1)
                .ok_or_else(|| Error::Invalid(alternative.into()))?;
            let full = precision(high) == 4;
            let below = if full {
                order(current, high) != Ordering::Greater
            } else {
                order(current, &upper) == Ordering::Less
            };
            if order(current, low) != Ordering::Less && below {
                return Ok(true);
            }
            continue;
        }
        let mut all = true;
        for clause in alternative
            .split([',', ' '])
            .filter(|c| !c.trim().is_empty())
        {
            if !satisfies(clause, current)? {
                all = false;
                break;
            }
        }
        if all {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Keep a single constraint's operator and precision; compound constraints
/// are kept while satisfied and otherwise not rewritten.
pub(super) fn new_range(current: &str, target: &str, policy: VersionPolicy) -> Result<String> {
    let parsed = parse(target)?;
    if policy == VersionPolicy::Exact {
        // Packagist reports tags (`v8.1.9`); constraints conventionally
        // omit the prefix unless the current one uses it.
        let keep = current.trim_start().starts_with(['v', 'V']);
        return Ok(if keep {
            target
        } else {
            target.trim_start_matches(['v', 'V'])
        }
        .into());
    }
    if matches(current, &parsed)? {
        return Ok(current.into());
    }
    let text = current.trim();
    if text.contains(['|', ',', ' ', '*']) || text.starts_with(['<', '!']) {
        return Err(Error::Compound(current.into()));
    }
    let n = text
        .find(|c: char| c.is_ascii_digit() || c == 'v' || c == 'V')
        .ok_or_else(|| Error::Invalid(current.into()))?;
    let (operator, base) = text.split_at(n);
    let prefix = if base.starts_with(['v', 'V']) {
        &base[..1]
    } else {
        ""
    };
    let digits = precision(base);
    let (numbers, rank, _) = key(target).ok_or_else(|| Error::Invalid(target.into()))?;
    let version = if rank == 4 && precision(target) >= digits {
        numbers[..digits]
            .iter()
            .map(u64::to_string)
            .collect::<Vec<_>>()
            .join(".")
    } else {
        target.trim_start_matches(['v', 'V']).to_string()
    };
    let out = format!("{operator}{prefix}{version}");
    if !matches(&out, &parsed)? {
        return Err(Error::Compound(current.into()));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn composer_order_constraints_and_rewrites() {
        use Ordering::*;
        for (a, b, expected) in [
            ("1.0.0-RC1", "1.0.0", Less),
            ("1.0.0-beta2", "1.0.0-RC1", Less),
            ("1.0.0", "1.0.0-patch1", Less),
            ("v6.4.1", "6.4.1", Equal),
            ("2.0.0.1", "2.0.0", Greater),
            ("1.10.0", "1.9.0", Greater),
        ] {
            assert_eq!(order(a, b), expected, "{a} vs {b}");
        }
        let v = |s| parse(s).unwrap();
        for (constraint, version, yes) in [
            ("^1.2", "1.9.0", true),
            ("^1.2", "2.0.0", false),
            ("^0.3", "0.3.9", true),
            ("^0.3", "0.4.0", false),
            ("~1.2", "1.9.0", true),
            ("~1.2", "2.0.0", false),
            ("~1.2.3", "1.2.9", true),
            ("~1.2.3", "1.3.0", false),
            ("1.2.*", "1.2.7", true),
            ("1.2.*", "1.3.0", false),
            (">=1.0 <2.0", "1.5.0", true),
            (">=1.0,<2.0", "2.0.0", false),
            ("^1.0 || ^3.0", "3.1.0", true),
            ("1.0 - 2.0", "2.0.9", true),
            ("1.0 - 2.0", "2.1.0", false),
            ("^2.0@dev", "2.1.0", true),
            ("*", "9.0.0", true),
        ] {
            assert_eq!(
                matches(constraint, &v(version)).unwrap(),
                yes,
                "{constraint} {version}"
            );
        }
        assert!(!v("1.0.0-RC1").0.pre.is_empty());
        assert!(v("1.0.0-patch1").0.pre.is_empty());
        for (old, new, expected) in [
            ("^6.4", "7.1.2", "^7.1"),
            ("~6.4.1", "6.5.0", "~6.5.0"),
            ("v2.1.0", "v2.3.0", "v2.3.0"),
            ("^1.2", "1.9.0", "^1.2"),
        ] {
            assert_eq!(
                new_range(old, new, VersionPolicy::Preserve).unwrap(),
                expected,
                "{old} -> {new}"
            );
        }
        assert!(new_range(">=1.0 <2.0", "3.0.0", VersionPolicy::Preserve).is_err());
        assert_eq!(
            new_range("^6.4", "v8.1.9", VersionPolicy::Exact).unwrap(),
            "8.1.9"
        );
        assert_eq!(
            new_range("v2.1.0", "v2.3.0", VersionPolicy::Exact).unwrap(),
            "v2.3.0"
        );
    }
}
