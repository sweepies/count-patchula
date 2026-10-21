//! RubyGems versions and requirements. Versions are dot-separated segments
//! split further at digit/letter boundaries; any letter makes a prerelease
//! (`1.0.0.rc1 < 1.0.0`), and numbers sort after letters. Requirements use
//! `=`, `!=`, `<`, `<=`, `>`, `>=` and the pessimistic `~>`.
use super::*;
use std::cmp::Ordering;

#[derive(Debug, PartialEq, Eq)]
enum Segment {
    Number(u64),
    Text(String),
}

fn segments(version: &str) -> Vec<Segment> {
    // A hyphen means a prerelease, as in `1.0.0-rc1` == `1.0.0.pre.rc1`.
    let version = version.trim().replace('-', ".pre.");
    let mut out = Vec::new();
    for part in version.split('.') {
        let mut token = String::new();
        let mut digits = None;
        for c in part.chars() {
            let is_digit = c.is_ascii_digit();
            if digits.is_some_and(|d| d != is_digit) {
                out.push(segment(std::mem::take(&mut token), digits.unwrap()));
            }
            digits = Some(is_digit);
            token.push(c);
        }
        if let Some(d) = digits {
            out.push(segment(token, d));
        }
    }
    out
}
fn segment(token: String, digits: bool) -> Segment {
    if digits {
        Segment::Number(token.parse().unwrap_or(u64::MAX))
    } else {
        Segment::Text(token.to_ascii_lowercase())
    }
}

pub(super) fn order(a: &str, b: &str) -> Ordering {
    let (a, b) = (segments(a), segments(b));
    for i in 0..a.len().max(b.len()) {
        let order = match (a.get(i), b.get(i)) {
            (Some(Segment::Number(x)), Some(Segment::Number(y))) => x.cmp(y),
            (Some(Segment::Text(x)), Some(Segment::Text(y))) => x.cmp(y),
            (Some(Segment::Text(_)), Some(Segment::Number(_))) => Ordering::Less,
            (Some(Segment::Number(_)), Some(Segment::Text(_))) => Ordering::Greater,
            // Missing segments pad with zero; a trailing letter is a prerelease.
            (Some(Segment::Number(x)), None) => x.cmp(&0),
            (None, Some(Segment::Number(y))) => 0.cmp(y),
            (Some(Segment::Text(_)), None) => Ordering::Less,
            (None, Some(Segment::Text(_))) => Ordering::Greater,
            (None, None) => Ordering::Equal,
        };
        if order != Ordering::Equal {
            return order;
        }
    }
    Ordering::Equal
}

pub(super) fn parse(value: &str) -> Result<Parsed> {
    let value = value.trim();
    if value.is_empty()
        || !value.starts_with(|c: char| c.is_ascii_digit())
        || !value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-')
    {
        return Err(Error::Invalid(value.into()));
    }
    let parts = segments(value);
    let numbers: Vec<u64> = parts
        .iter()
        .map_while(|s| match s {
            Segment::Number(n) => Some(*n),
            Segment::Text(_) => None,
        })
        .collect();
    let mut version = Version::new(
        *numbers.first().unwrap_or(&0),
        *numbers.get(1).unwrap_or(&0),
        *numbers.get(2).unwrap_or(&0),
    );
    if parts.iter().any(|s| matches!(s, Segment::Text(_))) {
        version.pre = semver::Prerelease::new("pre").expect("static prerelease");
    }
    Ok(Parsed(version, None, Some(value.into())))
}

/// The upper bound of `~> version`: drop the last numeric segment (and any
/// prerelease), then increment the new last one.
fn pessimistic_upper(version: &str) -> String {
    let numbers: Vec<u64> = segments(version)
        .iter()
        .map_while(|s| match s {
            Segment::Number(n) => Some(*n),
            Segment::Text(_) => None,
        })
        .collect();
    let mut upper: Vec<u64> = if numbers.len() > 1 {
        numbers[..numbers.len() - 1].to_vec()
    } else {
        numbers.clone()
    };
    if let Some(last) = upper.last_mut() {
        *last += 1;
    }
    upper
        .iter()
        .map(u64::to_string)
        .collect::<Vec<_>>()
        .join(".")
}

fn clause(text: &str) -> Result<(&str, &str)> {
    let text = text.trim();
    let end = text
        .find(|c: char| c.is_ascii_digit())
        .ok_or_else(|| Error::Invalid(text.into()))?;
    let (op, version) = text.split_at(end);
    let op = match op.trim() {
        "" => "=",
        op @ ("=" | "!=" | "<" | "<=" | ">" | ">=" | "~>") => op,
        _ => return Err(Error::Invalid(text.into())),
    };
    Ok((op, version.trim()))
}

pub(super) fn matches(requirement: &str, version: &Parsed) -> Result<bool> {
    let current = version.2.as_deref().expect("RubyGems parsed version");
    for part in requirement.split(',') {
        let (op, base) = clause(part)?;
        let order = order(current, base);
        let ok = match op {
            "=" => order.is_eq(),
            "!=" => !order.is_eq(),
            "<" => order.is_lt(),
            "<=" => !order.is_gt(),
            ">" => order.is_gt(),
            ">=" => !order.is_lt(),
            _ => !order.is_lt() && self::order(current, &pessimistic_upper(base)).is_lt(),
        };
        if !ok {
            return Ok(false);
        }
    }
    Ok(true)
}

/// Keep one requirement's operator and precision; compound requirements are
/// not rewritten.
pub(super) fn new_range(current: &str, target: &str, policy: VersionPolicy) -> Result<String> {
    let parsed = parse(target)?;
    if policy == VersionPolicy::Exact {
        return Ok(target.into());
    }
    if current.contains(',') {
        return if matches(current, &parsed)? {
            Ok(current.into())
        } else {
            Err(Error::Compound(current.into()))
        };
    }
    if matches(current, &parsed)? {
        return Ok(current.into());
    }
    let (op, base) = clause(current)?;
    if matches!(op, "<" | "<=" | "!=") {
        return Err(Error::Compound(current.into()));
    }
    let precision = base.split('.').count();
    let version = if parsed.0.pre.is_empty() && target.split('.').count() >= precision {
        target
            .split('.')
            .take(precision)
            .collect::<Vec<_>>()
            .join(".")
    } else {
        target.into()
    };
    let prefix = &current[..current.len() - current.trim_start().len()];
    let out = if op == "=" && !current.trim_start().starts_with('=') {
        format!("{prefix}{version}")
    } else {
        format!("{prefix}{op} {version}")
    };
    if !matches(&out, &parsed)? {
        return Err(Error::Compound(current.into()));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn gem_order_requirements_and_rewrites() {
        use Ordering::*;
        for (a, b, expected) in [
            ("1.0.0.rc1", "1.0.0", Less),
            ("1.0.0-rc1", "1.0.0.pre.rc1", Equal),
            ("8.1.3.1", "8.1.3", Greater),
            ("1.0", "1.0.0", Equal),
            ("1.10", "1.9", Greater),
            ("2.0.0.beta2", "2.0.0.beta10", Less),
            ("1.0.a", "1.0.0", Less),
        ] {
            assert_eq!(order(a, b), expected, "{a} vs {b}");
        }
        let v = |s| parse(s).unwrap();
        assert!(matches("~> 7.1", &v("7.9.0")).unwrap());
        assert!(!matches("~> 7.1", &v("8.0.0")).unwrap());
        assert!(matches("~> 7.1.2", &v("7.1.9")).unwrap());
        assert!(!matches("~> 7.1.2", &v("7.2.0")).unwrap());
        assert!(matches(">= 1.2, < 2", &v("1.9")).unwrap());
        assert!(!v("1.0.0.rc1").0.pre.is_empty());
        for (old, new, expected) in [
            ("~> 7.1", "8.0.2", "~> 8.0"),
            ("~> 7.1.2", "7.2.1", "~> 7.2.1"),
            ("7.1.0", "7.2.0", "7.2.0"),
            (">= 1.2", "3.0.0", ">= 1.2"),
        ] {
            assert_eq!(
                new_range(old, new, VersionPolicy::Preserve).unwrap(),
                expected,
                "{old} -> {new}"
            );
        }
    }
}
