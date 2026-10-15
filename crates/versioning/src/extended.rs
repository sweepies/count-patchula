use super::*;
use std::str::FromStr;

pub(super) fn parse(scheme: Ecosystem, value: &str) -> Result<Parsed> {
    let invalid = || Error::Invalid(value.into());
    if scheme == Ecosystem::Pep440 {
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
    let mut parsed = Ecosystem::Loose.parse(number)?;
    // Docker variants are compatibility labels, not semver prereleases.
    parsed.2 = Some(suffix.into());
    Ok(parsed)
}

pub(super) fn matches(scheme: Ecosystem, range: &str, version: &Parsed) -> Result<bool> {
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
    Ok(format!("{operator}{target}"))
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
}
