//! NuGet versions: SemVer 2 with an optional fourth release number. Release
//! numbers compare numerically, prerelease labels case-insensitively, and
//! build metadata is ignored. Ranges use Maven's interval notation.
use super::*;
use std::cmp::Ordering;

fn split(version: &str) -> (Vec<u64>, Option<&str>) {
    let version = version.trim().split('+').next().unwrap_or("");
    let (release, pre) = match version.split_once('-') {
        Some((r, p)) => (r, Some(p)),
        None => (version, None),
    };
    let mut numbers: Vec<u64> = release.split('.').map(|p| p.parse().unwrap_or(0)).collect();
    numbers.resize(4, 0);
    (numbers, pre)
}

pub(super) fn order(a: &str, b: &str) -> Ordering {
    let ((ra, pa), (rb, pb)) = (split(a), split(b));
    ra.cmp(&rb).then_with(|| match (pa, pb) {
        (None, None) => Ordering::Equal,
        (None, Some(_)) => Ordering::Greater,
        (Some(_), None) => Ordering::Less,
        (Some(pa), Some(pb)) => {
            let (mut ia, mut ib) = (pa.split('.'), pb.split('.'));
            loop {
                match (ia.next(), ib.next()) {
                    (None, None) => return Ordering::Equal,
                    (None, Some(_)) => return Ordering::Less,
                    (Some(_), None) => return Ordering::Greater,
                    (Some(x), Some(y)) => {
                        let order = match (x.parse::<u64>(), y.parse::<u64>()) {
                            (Ok(x), Ok(y)) => x.cmp(&y),
                            (Ok(_), Err(_)) => Ordering::Less,
                            (Err(_), Ok(_)) => Ordering::Greater,
                            (Err(_), Err(_)) => x.to_ascii_lowercase().cmp(&y.to_ascii_lowercase()),
                        };
                        if order != Ordering::Equal {
                            return order;
                        }
                    }
                }
            }
        }
    })
}

pub(super) fn parse(value: &str) -> Result<Parsed> {
    let value = value.trim();
    let invalid = || Error::Invalid(value.into());
    let release = value.split(['-', '+']).next().unwrap_or("");
    let parts: Vec<&str> = release.split('.').collect();
    if parts.is_empty()
        || parts.len() > 4
        || parts
            .iter()
            .any(|p| p.is_empty() || !p.bytes().all(|b| b.is_ascii_digit()))
    {
        return Err(invalid());
    }
    let number = |i: usize| {
        parts
            .get(i)
            .map_or(Ok(0), |p| p.parse().map_err(|_| invalid()))
    };
    let mut version = Version::new(number(0)?, number(1)?, number(2)?);
    if split(value).1.is_some() {
        version.pre = semver::Prerelease::new("pre").expect("static prerelease");
    }
    Ok(Parsed(version, None, Some(value.into())))
}

pub(super) fn matches(range: &str, version: &Parsed) -> Result<bool> {
    let current = version.2.as_deref().expect("NuGet parsed version");
    let range = range.trim();
    if !range.starts_with(['[', '(']) {
        return Ok(order(range, current).is_eq());
    }
    crate::maven::intervals(range, current, order)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn semver2_with_a_fourth_number_and_case_insensitive_labels() {
        use Ordering::*;
        for (a, b, expected) in [
            ("9.0.0-preview.1", "9.0.0", Less),
            ("9.0.0-preview.2", "9.0.0-preview.10", Less),
            ("9.0.0-rc.1", "9.0.0-preview.9", Greater),
            ("1.0.0-Beta", "1.0.0-beta", Equal),
            ("4.0.0.1", "4.0.0", Greater),
            ("4.0", "4.0.0.0", Equal),
            ("1.0.0+build", "1.0.0", Equal),
            ("13.0.4", "13.0.10", Less),
        ] {
            assert_eq!(order(a, b), expected, "{a} vs {b}");
        }
        assert!(matches("[8.0,9.0)", &parse("8.0.11").unwrap()).unwrap());
        assert!(!matches("[8.0,9.0)", &parse("9.0.0").unwrap()).unwrap());
        assert!(!parse("9.0.0-rc.1").unwrap().0.pre.is_empty());
        assert!(parse("1.*").is_err());
    }
}
