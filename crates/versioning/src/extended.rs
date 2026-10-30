use super::*;
use std::str::FromStr;

pub(super) fn parse(scheme: Ecosystem, value: &str) -> Result<Parsed> {
    let invalid = || Error::Invalid(value.into());
    if matches!(scheme, Ecosystem::Pep440 | Ecosystem::Poetry) {
        let pep = pep440_rs::Version::from_str(value).map_err(|_| invalid())?;
        let r = pep.release();
        let mut v = Version::new(r[0], *r.get(1).unwrap_or(&0), *r.get(2).unwrap_or(&0));
        if pep.is_pre() || pep.is_dev() {
            v.pre = semver::Prerelease::new("pre").expect("static prerelease");
        }
        return Ok(Parsed(v, Some(pep), None));
    }
    let value = value.trim().trim_start_matches('v');
    if scheme == Ecosystem::Hashicorp {
        let parsed = Ecosystem::Loose.parse(value)?;
        return Ok(parsed);
    }
    let end = value
        .find(|c: char| !c.is_ascii_digit() && c != '.')
        .unwrap_or(value.len());
    let number = &value[..end];
    if number.is_empty() || number.ends_with('.') {
        return Err(invalid());
    }
    let suffix = &value[end..];
    if !suffix.is_empty() && !suffix.starts_with('-') {
        return Err(invalid());
    }
    let numeric = pep440_rs::Version::from_str(number).map_err(|_| invalid())?;
    let r = numeric.release();
    let mut parsed = Parsed(
        Version::new(r[0], *r.get(1).unwrap_or(&0), *r.get(2).unwrap_or(&0)),
        Some(numeric),
        None,
    );
    // Docker variants are compatibility labels, not semver prereleases.
    parsed.2 = Some(suffix.into());
    Ok(parsed)
}

pub(super) fn matches(scheme: Ecosystem, range: &str, version: &Parsed) -> Result<bool> {
    if scheme == Ecosystem::Poetry {
        let version = version.1.as_ref().expect("PEP parsed version");
        for alternative in range.split("||") {
            let specifiers = poetry_specifiers(alternative)?;
            if pep440_rs::VersionSpecifiers::from_str(&specifiers)
                .map_err(|_| Error::Invalid(range.into()))?
                .contains(version)
            {
                return Ok(true);
            }
        }
        return Ok(false);
    }
    if scheme == Ecosystem::Docker {
        let current = scheme.parse(range)?;
        return Ok(current.2 == version.2 && scheme.order(&current, version).is_eq());
    }
    if scheme == Ecosystem::Pep440 {
        let text = if range.starts_with(|c: char| c.is_ascii_digit() || c == 'v') {
            format!("=={range}")
        } else {
            range.into()
        };
        return pep440_rs::VersionSpecifiers::from_str(&text)
            .map(|r| r.contains(version.1.as_ref().expect("PEP parsed version")))
            .map_err(|_| Error::Invalid(range.into()));
    }
    for clause in range.split(',') {
        let clause = clause.trim();
        let op_end = clause
            .find(|c: char| !matches!(c, '~' | '>' | '<' | '=' | '!'))
            .unwrap_or(clause.len());
        let (op, body) = clause.split_at(op_end);
        let base = scheme.parse(body.trim())?;
        let order = scheme.order(version, &base);
        let yes = match op {
            "" | "=" | "==" => order.is_eq(),
            "!=" => !order.is_eq(),
            ">" => order.is_gt(),
            ">=" => !order.is_lt(),
            "<" => order.is_lt(),
            "<=" => !order.is_gt(),
            "~>" => {
                let mut upper = base.clone();
                if body.trim().split('.').count() >= 3 {
                    upper.0.minor += 1;
                    upper.0.patch = 0;
                } else {
                    upper.0.major += 1;
                    upper.0.minor = 0;
                    upper.0.patch = 0;
                }
                !order.is_lt() && scheme.order(version, &upper).is_lt()
            }
            _ => return Err(Error::Invalid(range.into())),
        };
        if !yes {
            return Ok(false);
        }
    }
    Ok(true)
}

pub(super) fn new_range(
    scheme: Ecosystem,
    current: &str,
    target: &str,
    policy: VersionPolicy,
) -> Result<String> {
    let parsed = scheme.parse(target)?;
    if scheme == Ecosystem::Docker {
        if let Ok(base) = scheme.parse(current)
            && base.2 != parsed.2
        {
            return Err(Error::Invalid(target.into()));
        }
        return Ok(target.into());
    }
    if policy != VersionPolicy::Exact && scheme.matches(current, &parsed)? {
        return Ok(current.into());
    }
    let exact = if scheme == Ecosystem::Pep440 {
        format!("=={target}")
    } else {
        target.into()
    };
    if policy == VersionPolicy::Exact {
        return Ok(exact);
    }
    if scheme == Ecosystem::Poetry {
        return if policy == VersionPolicy::Widen {
            poetry_widen(current, target, &parsed)
        } else {
            poetry_range(current, target, &parsed)
        };
    }
    let clauses = current.split(',').collect::<Vec<_>>();
    if clauses.len() != 1 {
        return Err(Error::Compound(current.into()));
    }
    let n = current
        .find(|c: char| c.is_ascii_digit() || c == 'v')
        .ok_or_else(|| Error::Invalid(current.into()))?;
    let operator = current[..n].trim();
    if policy == VersionPolicy::Widen && matches!(operator, "<" | "<=") {
        return Ok(format!("<={target}"));
    }
    // Keep the precision the declaration had: `>=2.28` shifts to `>=2.32`, not
    // `>=2.32.3`.
    let body = current[n..].trim();
    let precision = body.split('.').count();
    let shortened = (!body.contains('*')
        && target.bytes().all(|b| b.is_ascii_digit() || b == b'.')
        && target.split('.').count() >= precision)
        .then(|| {
            target
                .split('.')
                .take(precision)
                .collect::<Vec<_>>()
                .join(".")
        });
    // `==2.32` does not match 2.32.3, so an operator that needs the full
    // version gets it.
    for version in shortened.iter().map(String::as_str).chain([target]) {
        let out = format!("{}{version}", &current[..n]);
        if scheme.matches(&out, &parsed)? {
            return Ok(out);
        }
    }
    Err(Error::Compound(current.into()))
}

/// One Poetry alternative (comma-separated clauses) as PEP 440 specifiers.
/// Caret allows changes that keep the leftmost nonzero component; tilde allows
/// patch changes, or minor changes for a bare major.
fn poetry_specifiers(alternative: &str) -> Result<String> {
    let mut out = Vec::new();
    for clause in alternative.split(',') {
        let clause = clause.trim();
        let invalid = || Error::Invalid(clause.into());
        if clause.is_empty() {
            return Err(invalid());
        }
        if clause == "*" {
            out.push(">=0".to_string());
            continue;
        }
        let (op, body) = if let Some(body) = clause.strip_prefix('^') {
            ("^", body.trim())
        } else if let Some(body) = clause.strip_prefix('~').filter(|b| !b.starts_with('=')) {
            ("~", body.trim())
        } else if clause.starts_with(|c: char| c.is_ascii_digit()) {
            out.push(format!("=={clause}"));
            continue;
        } else {
            out.push(clause.to_string());
            continue;
        };
        // Bounds come from the release segment; prerelease suffixes stay in the floor.
        let parts = pep440_rs::Version::from_str(body)
            .map_err(|_| invalid())?
            .release()
            .to_vec();
        if parts.is_empty() || parts.len() > 3 {
            return Err(invalid());
        }
        let bump = if op == "^" {
            parts
                .iter()
                .position(|p| *p != 0)
                .unwrap_or_else(|| parts.len() - 1)
        } else if parts.len() == 1 {
            0
        } else {
            1
        };
        let mut upper: Vec<u64> = (0..=bump).map(|i| *parts.get(i).unwrap_or(&0)).collect();
        upper[bump] += 1;
        let upper = upper
            .iter()
            .map(u64::to_string)
            .collect::<Vec<_>>()
            .join(".");
        out.push(format!(">={body}"));
        out.push(format!("<{upper}"));
    }
    Ok(out.join(","))
}

/// Add an alternative that covers the target to the constraint, which `||`
/// allows: `^1.2` and 3.0.1 become `^1.2 || ^3.0`. Any constraint can be
/// extended this way.
fn poetry_widen(current: &str, target: &str, parsed: &Parsed) -> Result<String> {
    let release = parsed.1.as_ref().expect("PEP parsed version");
    let plain =
        !release.is_pre() && !release.is_post() && !release.is_dev() && release.local().is_empty();
    let numbers = release.release();
    let (major, minor, patch) = (
        numbers[0],
        numbers.get(1).copied().unwrap_or(0),
        numbers.get(2).copied().unwrap_or(0),
    );
    let caret = if !plain {
        format!("^{target}")
    } else if major > 0 {
        format!("^{major}.{minor}")
    } else if minor > 0 {
        format!("^0.{minor}")
    } else {
        format!("^0.0.{patch}")
    };
    let out = format!("{} || {caret}", current.trim());
    if !Ecosystem::Poetry.matches(&out, parsed)? {
        return Err(Error::Compound(current.into()));
    }
    Ok(out)
}

/// Keep a single caret, tilde, comparison or exact constraint's operator and
/// precision. Compound constraints cannot be rewritten safely.
fn poetry_range(current: &str, target: &str, parsed: &Parsed) -> Result<String> {
    let text = current.trim();
    if text.contains([',', '|']) || text == "*" {
        return Err(Error::Compound(current.into()));
    }
    let n = text
        .find(|c: char| c.is_ascii_digit())
        .ok_or_else(|| Error::Invalid(current.into()))?;
    let (operator, body) = text.split_at(n);
    let precision = body.split('.').count();
    let release = parsed.1.as_ref().expect("PEP parsed version");
    let plain =
        !release.is_pre() && !release.is_post() && !release.is_dev() && release.local().is_empty();
    let version = if plain && release.release().len() >= precision && !body.contains('*') {
        release.release()[..precision]
            .iter()
            .map(u64::to_string)
            .collect::<Vec<_>>()
            .join(".")
    } else {
        target.to_string()
    };
    let out = format!("{}{version}", operator.trim());
    if !Ecosystem::Poetry.matches(&out, parsed)? {
        return Err(Error::Compound(current.into()));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn pep440_order_and_specifiers() {
        let s = Ecosystem::Pep440;
        let versions = [
            "1.0.dev1",
            "1.0a1",
            "1.0b1",
            "1.0rc1",
            "1.0",
            "1.0+abc",
            "1.0.post1",
            "1!0.1",
        ];
        for pair in versions.windows(2) {
            assert!(
                s.order(&s.parse(pair[0]).unwrap(), &s.parse(pair[1]).unwrap())
                    .is_lt()
            );
        }
        for (range, v, yes) in [
            ("~=1.4.5", "1.4.9", true),
            ("~=1.4.5", "1.5", false),
            ("==1.0", "1.0+local", true),
            ("!=1.2.*,>=1", "1.2.4", false),
        ] {
            assert_eq!(s.matches(range, &s.parse(v).unwrap()).unwrap(), yes);
        }
    }
    #[test]
    fn docker_variants_and_hashicorp_pessimistic_constraints() {
        let d = Ecosystem::Docker;
        assert!(
            d.order(
                &d.parse("10.0.20348.4052-windows").unwrap(),
                &d.parse("10.0.20348.4000-windows").unwrap()
            )
            .is_gt()
        );
        assert_eq!(
            d.parse("v3.21-alpine").unwrap().2.as_deref(),
            Some("-alpine")
        );
        assert!(
            d.new_range("3.20-alpine", "3.21-bookworm", VersionPolicy::Exact)
                .is_err()
        );
        assert_eq!(
            d.new_range("3.20-alpine", "3.21-alpine", VersionPolicy::Exact)
                .unwrap(),
            "3.21-alpine"
        );
        let h = Ecosystem::Hashicorp;
        assert!(
            h.matches("~> 1.2.0, !=1.2.5", &h.parse("1.2.6").unwrap())
                .unwrap()
        );
        assert!(!h.matches("~> 1.2.0", &h.parse("1.3.0").unwrap()).unwrap());
        assert!(h.matches("~> 1.2", &h.parse("1.9.0").unwrap()).unwrap());
    }
    #[test]
    fn poetry_caret_tilde_and_alternatives() {
        let p = Ecosystem::Poetry;
        for (range, v, yes) in [
            ("^1.2.3", "1.9.0", true),
            ("^1.2.3", "2.0.0", false),
            ("^0.2.3", "0.2.9", true),
            ("^0.2.3", "0.3.0", false),
            ("^0.0.3", "0.0.4", false),
            ("^0", "0.9.0", true),
            ("^0.0", "0.1.0", false),
            ("~1.2.3", "1.2.9", true),
            ("~1.2.3", "1.3.0", false),
            ("~1.2", "1.2.5", true),
            ("~1", "1.9.0", true),
            ("~1", "2.0.0", false),
            ("~=1.4.5", "1.4.9", true),
            ("1.2.*", "1.2.7", true),
            ("1.2.3", "1.2.3", true),
            (">=1.2, <1.5", "1.4.0", true),
            (">=1.2, <1.5", "1.5.0", false),
            ("^1.0 || ^3.0", "3.1.0", true),
            ("*", "9.0.0", true),
        ] {
            assert_eq!(
                p.matches(range, &p.parse(v).unwrap()).unwrap(),
                yes,
                "{range} {v}"
            );
        }
        for (old, new, expected) in [
            ("^0.27.0", "0.28.1", "^0.28.1"),
            ("^1.2", "2.3.4", "^2.3"),
            ("~2.8.0", "2.9.1", "~2.9.1"),
            ("0.6.0", "0.6.9", "0.6.9"),
            ("==1.0", "2.1.0", "==2.1"),
            ("^1.0", "2.0.0rc1", "^2.0.0rc1"),
        ] {
            assert_eq!(
                p.new_range(old, new, VersionPolicy::Preserve).unwrap(),
                expected,
                "{old} -> {new}"
            );
        }
        assert_eq!(
            p.new_range("^1.2", "1.9.0", VersionPolicy::Preserve)
                .unwrap(),
            "^1.2"
        );
        assert!(
            p.new_range(">=1.2, <2", "3.0.0", VersionPolicy::Preserve)
                .is_err()
        );
    }
}
