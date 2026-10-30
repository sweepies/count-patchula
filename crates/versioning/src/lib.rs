//! Ecosystem version semantics. npm ranges are deliberately not Cargo requirements.
#![forbid(unsafe_code)]

use patchula_config::{Update, VersionPolicy};
use semver::{Version, VersionReq};
use std::cmp::Ordering;
mod composer;
mod extended;
mod maven;
mod nuget;
mod rubygems;
pub use maven::variant as maven_variant;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ecosystem {
    Npm,
    Cargo,
    Loose,
    Go,
    Docker,
    Pep440,
    /// Poetry constraints: PEP 440 versions with `^`/`~` ranges and `||`.
    Poetry,
    /// Maven's version order; bare versions are exact requirements.
    Maven,
    /// NuGet's SemVer 2 with an optional fourth release number.
    Nuget,
    /// RubyGems versions and `~>` requirements.
    Rubygems,
    /// Composer versions and constraints.
    Composer,
    Hashicorp,
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("invalid version or requirement: {0}")]
    Invalid(String),
    #[error("cannot safely preserve this compound requirement: {0}; use exact or widen")]
    Compound(String),
}
pub type Result<T> = std::result::Result<T, Error>;

/// Parsed once, reused while sorting/filtering a package's releases.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Parsed(
    pub Version,
    pub Option<pep440_rs::Version>,
    pub Option<String>,
);

pub trait Versioning: Send + Sync {
    fn parse(&self, value: &str) -> Result<Parsed>;
    fn order(&self, left: &Parsed, right: &Parsed) -> Ordering;
    fn matches(&self, range: &str, version: &Parsed) -> Result<bool>;
    fn classify(&self, current: &Parsed, target: &Parsed) -> Update;
    fn new_range(&self, current: &str, target: &str, policy: VersionPolicy) -> Result<String>;
}

impl Versioning for Ecosystem {
    fn parse(&self, value: &str) -> Result<Parsed> {
        if *self == Self::Maven {
            return maven::parse(value);
        }
        if *self == Self::Nuget {
            return nuget::parse(value);
        }
        if *self == Self::Rubygems {
            return rubygems::parse(value);
        }
        if *self == Self::Composer {
            return composer::parse(value);
        }
        if matches!(
            self,
            Self::Docker | Self::Pep440 | Self::Poetry | Self::Hashicorp
        ) {
            return extended::parse(*self, value);
        }
        let value = value.trim();
        let value = match self {
            Self::Cargo => value,
            Self::Go => value
                .strip_prefix('v')
                .ok_or_else(|| Error::Invalid(value.into()))?,
            _ => value.trim_start_matches('v'),
        };
        let normalized;
        let value = if matches!(self, Self::Loose) && !value.contains(['-', '+']) {
            let components: Vec<_> = value
                .split('.')
                .map(|v| v.parse::<u64>())
                .collect::<std::result::Result<_, _>>()
                .map_err(|_| Error::Invalid(value.into()))?;
            let fields = components.len();
            let value = components
                .iter()
                .map(u64::to_string)
                .collect::<Vec<_>>()
                .join(".");
            normalized = match fields {
                1 => format!("{value}.0.0"),
                2 => format!("{value}.0"),
                _ => value,
            };
            normalized.as_str()
        } else {
            value
        };
        Version::parse(value)
            .map(|v| Parsed(v, None, None))
            .map_err(|_| Error::Invalid(value.into()))
    }
    fn order(&self, left: &Parsed, right: &Parsed) -> Ordering {
        if *self == Self::Maven
            && let (Some(a), Some(b)) = (&left.2, &right.2)
        {
            return maven::order(a, b);
        }
        if *self == Self::Nuget
            && let (Some(a), Some(b)) = (&left.2, &right.2)
        {
            return nuget::order(a, b);
        }
        if *self == Self::Rubygems
            && let (Some(a), Some(b)) = (&left.2, &right.2)
        {
            return rubygems::order(a, b);
        }
        if *self == Self::Composer
            && let (Some(a), Some(b)) = (&left.2, &right.2)
        {
            return composer::order(a, b);
        }
        if let (Some(a), Some(b)) = (&left.1, &right.1) {
            return a.cmp(b);
        }
        // SemVer precedence ignores build metadata, including Go +incompatible.
        left.0.cmp_precedence(&right.0)
    }
    fn matches(&self, range: &str, version: &Parsed) -> Result<bool> {
        match self {
            Self::Npm => {
                let range =
                    nodejs_semver::Range::parse(range).map_err(|_| Error::Invalid(range.into()))?;
                let version = nodejs_semver::Version::parse(version.0.to_string())
                    .map_err(|_| Error::Invalid(version.0.to_string()))?;
                Ok(range.satisfies(&version))
            }
            Self::Cargo => VersionReq::parse(range)
                .map(|req| req.matches(&version.0))
                .map_err(|_| Error::Invalid(range.into())),
            Self::Maven => maven::matches(range, version),
            Self::Nuget => nuget::matches(range, version),
            Self::Rubygems => rubygems::matches(range, version),
            Self::Composer => composer::matches(range, version),
            Self::Docker | Self::Pep440 | Self::Poetry | Self::Hashicorp => {
                extended::matches(*self, range, version)
            }
            Self::Loose | Self::Go => self
                .parse(range)
                .map(|v| self.order(&v, version) == Ordering::Equal),
        }
    }
    fn classify(&self, current: &Parsed, target: &Parsed) -> Update {
        let (a, b) = (&current.0, &target.0);
        if current
            .1
            .as_ref()
            .zip(target.1.as_ref())
            .is_some_and(|(a, b)| a.epoch() != b.epoch())
            || a.major != b.major
            || (*self == Self::Cargo
                && a.major == 0
                && (a.minor != b.minor || (a.minor == 0 && a.patch != b.patch)))
        {
            Update::Major
        } else if a.minor != b.minor {
            Update::Minor
        } else {
            Update::Patch
        }
    }
    fn new_range(&self, current: &str, target: &str, policy: VersionPolicy) -> Result<String> {
        if *self == Self::Rubygems {
            return rubygems::new_range(current, target, policy);
        }
        if *self == Self::Composer {
            return composer::new_range(current, target, policy);
        }
        if matches!(self, Self::Maven | Self::Nuget) {
            // A bare Maven or NuGet version is an exact (soft) requirement. Ranges keep
            // themselves when satisfied and are never rewritten.
            let parsed = self.parse(target)?;
            if !current.trim_start().starts_with(['[', '(']) || policy == VersionPolicy::Exact {
                return Ok(target.into());
            }
            if self.matches(current, &parsed)? {
                return Ok(current.into());
            }
            return if policy == VersionPolicy::Widen {
                maven::widen(current, target).ok_or_else(|| Error::Compound(current.into()))
            } else {
                Err(Error::Compound(current.into()))
            };
        }
        if matches!(
            self,
            Self::Docker | Self::Pep440 | Self::Poetry | Self::Hashicorp
        ) {
            return extended::new_range(*self, current, target, policy);
        }
        let parsed = self.parse(target)?;
        let prefix = if current.starts_with('v') { "v" } else { "" };
        let exact = if *self == Self::Cargo {
            format!("={}", parsed.0)
        } else {
            format!("{prefix}{}", parsed.0)
        };
        if policy == VersionPolicy::Exact || matches!(self, Self::Loose | Self::Go) {
            return Ok(exact);
        }
        if self.matches(current, &parsed)? {
            return Ok(current.into());
        }
        if policy == VersionPolicy::Widen {
            return match self {
                Self::Npm => Ok(format!("{current} || {exact}")),
                Self::Cargo => {
                    // A conjunction cannot express disjoint Cargo intervals. Expand
                    // the upper bound while retaining the original minimum.
                    let req =
                        VersionReq::parse(current).map_err(|_| Error::Invalid(current.into()))?;
                    let minimum = req
                        .comparators
                        .iter()
                        .filter(|c| {
                            matches!(
                                c.op,
                                semver::Op::Caret
                                    | semver::Op::Tilde
                                    | semver::Op::GreaterEq
                                    | semver::Op::Exact
                                    | semver::Op::Wildcard
                            )
                        })
                        .map(|c| (c.major, c.minor.unwrap_or(0), c.patch.unwrap_or(0)))
                        .min()
                        .ok_or_else(|| Error::Compound(current.into()))?;
                    let maximum = (parsed.0.major, parsed.0.minor, parsed.0.patch);
                    let minimum = minimum.min(maximum);
                    let upper = if parsed.0.major > 0 {
                        format!("{}.0.0", parsed.0.major + 1)
                    } else if parsed.0.minor > 0 {
                        format!("0.{}.0", parsed.0.minor + 1)
                    } else {
                        format!("0.0.{}", parsed.0.patch + 1)
                    };
                    Ok(format!(
                        ">={}.{}.{}, <{upper}",
                        minimum.0, minimum.1, minimum.2
                    ))
                }
                _ => unreachable!(),
            };
        }
        let text = current.trim();
        let operator = if text.starts_with(['^', '~', '=']) {
            &text[..1]
        } else {
            ""
        };
        let body = text
            .strip_prefix(operator)
            .unwrap_or(text)
            .trim_start()
            .trim_start_matches('v');
        if body.contains([' ', ',', '|', '>', '<']) {
            return Err(Error::Compound(current.into()));
        }
        let fields: Vec<_> = body.split('.').collect();
        let mut out = format!("{operator}{prefix}{}", parsed.0.major);
        if fields.len() >= 2 {
            out.push('.');
            out.push_str(if matches!(fields[1], "x" | "X" | "*") {
                fields[1]
            } else {
                ""
            });
            if !matches!(fields[1], "x" | "X" | "*") {
                // A bare Cargo requirement already includes compatible newer
                // releases. Across numeric majors start at that major's floor.
                let minor = if *self == Self::Cargo
                    && operator.is_empty()
                    && fields.len() == 2
                    && parsed.0.major > 0
                {
                    0
                } else {
                    parsed.0.minor
                };
                out.push_str(&minor.to_string());
            }
        }
        if fields.len() >= 3 {
            out.push('.');
            if matches!(fields[2], "x" | "X" | "*") {
                out.push_str(fields[2]);
            } else {
                out.push_str(&parsed.0.patch.to_string());
                if !parsed.0.pre.is_empty() {
                    out.push('-');
                    out.push_str(parsed.0.pre.as_str());
                }
            }
        }
        if !self.matches(&out, &parsed)? {
            return Err(Error::Compound(current.into()));
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn npm_documented_ranges() {
        for (range, version, expected) in [
            ("^0.2.3", "0.2.9", true),
            ("^0.2.3", "0.3.0", false),
            ("^0.0.3", "0.0.4", false),
            ("~1.2.3", "1.3.0", false),
            ("1.2.x", "1.2.9", true),
            ("1.x || >=2.5.0", "2.6.0", true),
            ("1.2.3 - 2.3", "2.3.99", true),
            ("^1.2.3", "1.3.0-beta.1", false),
            ("^1.2.3-beta.1", "1.2.3-beta.2", true),
            ("1.2.3", "1.2.3+build.8", true),
        ] {
            assert_eq!(
                Ecosystem::Npm
                    .matches(range, &Ecosystem::Npm.parse(version).unwrap())
                    .unwrap(),
                expected,
                "{range} {version}"
            );
        }
    }
    #[test]
    fn cargo_is_not_npm() {
        let v = Ecosystem::Cargo.parse("1.9.0").unwrap();
        assert!(Ecosystem::Cargo.matches("1.2.3", &v).unwrap());
        assert!(!Ecosystem::Npm.matches("1.2.3", &v).unwrap());
        assert_eq!(
            Ecosystem::Cargo.classify(
                &Ecosystem::Cargo.parse("0.2.3").unwrap(),
                &Ecosystem::Cargo.parse("0.3.0").unwrap()
            ),
            Update::Major
        );
        assert_eq!(
            Ecosystem::Npm.classify(
                &Ecosystem::Npm.parse("0.2.3").unwrap(),
                &Ecosystem::Npm.parse("0.3.0").unwrap()
            ),
            Update::Minor
        );
        assert!(
            !Ecosystem::Cargo
                .matches("*", &Ecosystem::Cargo.parse("1.0.0-alpha").unwrap())
                .unwrap()
        );
    }
    #[test]
    fn precedence_and_range_edits() {
        use VersionPolicy::*;
        assert_eq!(
            Ecosystem::Npm.order(
                &Ecosystem::Npm.parse("1.0.0+a").unwrap(),
                &Ecosystem::Npm.parse("1.0.0+b").unwrap()
            ),
            Ordering::Equal
        );
        for (scheme, old, new, policy, expected) in [
            (Ecosystem::Npm, "^0.2.3", "0.3.1", Preserve, "^0.3.1"),
            (Ecosystem::Npm, "1.2.x", "2.3.4", Preserve, "2.3.x"),
            (Ecosystem::Npm, "~1.2", "2.4.0", Preserve, "~2.4"),
            (Ecosystem::Npm, "^1.0.0", "2.0.0", Widen, "^1.0.0 || 2.0.0"),
            (Ecosystem::Cargo, "1.2", "2.3.4", Exact, "=2.3.4"),
            (Ecosystem::Go, "v1.2.3", "v1.3.0", Exact, "v1.3.0"),
        ] {
            assert_eq!(scheme.new_range(old, new, policy).unwrap(), expected);
        }
        assert!(
            Ecosystem::Npm
                .new_range(">=1 <2", "3.0.0", Preserve)
                .is_err()
        );
        assert!(
            Ecosystem::Go
                .parse("v0.0.0-20240101000000-abcdef123456")
                .is_ok()
        );
    }
    #[test]
    fn widen_extends_where_the_syntax_can_express_it() {
        use VersionPolicy::Widen;
        for (scheme, old, new, expected) in [
            (Ecosystem::Composer, "^1.2", "3.0.1", "^1.2 || ^3.0"),
            (
                Ecosystem::Composer,
                "^1.2 || ^3.0",
                "4.2.0",
                "^1.2 || ^3.0 || ^4.2",
            ),
            (Ecosystem::Composer, "^0.3", "0.5.2", "^0.3 || ^0.5"),
            (
                Ecosystem::Composer,
                ">=1.0 <2.0",
                "3.1.0",
                ">=1.0 <2.0 || ^3.1",
            ),
            (Ecosystem::Composer, "^1.2", "1.9.0", "^1.2"),
            (Ecosystem::Poetry, "^1.2", "3.0.1", "^1.2 || ^3.0"),
            (Ecosystem::Poetry, ">=1.2, <2", "2.4.0", ">=1.2, <2 || ^2.4"),
            (Ecosystem::Poetry, "^0.2.3", "0.4.1", "^0.2.3 || ^0.4"),
            (Ecosystem::Poetry, "^1.2", "1.9.0", "^1.2"),
            (Ecosystem::Maven, "[1.0,2.0)", "3.1", "[1.0,3.1]"),
            (Ecosystem::Maven, "(,2.0)", "2.5", "(,2.5]"),
            (Ecosystem::Maven, "[1.0]", "1.5", "[1.0,1.5]"),
            (Ecosystem::Maven, "[1.0,2.0)", "1.5", "[1.0,2.0)"),
            (Ecosystem::Nuget, "[2.9.0,3.0)", "3.2.0", "[2.9.0,3.2.0]"),
            (Ecosystem::Nuget, "(1.0,2.0]", "2.1.0", "(1.0,2.1.0]"),
        ] {
            let extended = scheme.new_range(old, new, Widen).unwrap();
            assert_eq!(extended, expected, "{scheme:?} {old} -> {new}");
            assert!(
                scheme
                    .matches(&extended, &scheme.parse(new).unwrap())
                    .unwrap(),
                "{scheme:?} {extended} must allow {new}"
            );
        }
        // Several intervals have no one bound to move.
        for scheme in [Ecosystem::Maven, Ecosystem::Nuget] {
            assert!(scheme.new_range("(,1.0],[1.2,1.5)", "2.0", Widen).is_err());
        }
        // A single declared version has nothing to extend: these schemes write
        // the same text under `widen` as under `preserve`.
        for (scheme, old, new) in [
            (Ecosystem::Rubygems, "~> 7.1", "8.0.2"),
            (Ecosystem::Rubygems, ">= 1.2, < 2", "3.0.0"),
            (Ecosystem::Docker, "3.20-alpine", "3.21-alpine"),
            (Ecosystem::Go, "v1.2.3", "v1.3.0"),
            (Ecosystem::Loose, "1.2.3", "1.3.0"),
        ] {
            assert_eq!(
                scheme.new_range(old, new, Widen).ok(),
                scheme.new_range(old, new, VersionPolicy::Preserve).ok(),
                "{scheme:?} {old} -> {new}"
            );
        }
    }

    #[test]
    fn shifted_ranges_keep_the_declared_precision() {
        use VersionPolicy::Preserve;
        for (scheme, old, new, expected) in [
            (Ecosystem::Pep440, "~=2.28.1", "2.32.3", "~=2.32.3"),
            (Ecosystem::Pep440, "<=2.28", "2.32.3", "<=2.32.3"),
            (Ecosystem::Pep440, "~=2.28", "3.1.4", "~=3.1"),
            (Ecosystem::Pep440, "~=2.28.1", "2.29.3", "~=2.29.3"),
            (Ecosystem::Pep440, "==2.28", "2.32.3", "==2.32.3"),
            (Ecosystem::Pep440, "~=2.28", "3.0.0rc1", "~=3.0.0rc1"),
            (Ecosystem::Hashicorp, "~> 6.0", "7.1.2", "~> 7.1"),
            (Ecosystem::Hashicorp, "~> 1.2.0", "1.3.4", "~> 1.3.4"),
            (Ecosystem::Hashicorp, "= 1.2", "2.4.7", "= 2.4.7"),
            (Ecosystem::Hashicorp, "~> 1", "2.4.7", "~> 2"),
        ] {
            assert_eq!(
                scheme.new_range(old, new, Preserve).unwrap(),
                expected,
                "{scheme:?} {old} -> {new}"
            );
        }
        // A bound the new version cannot satisfy is not rewritten by guessing.
        assert!(
            Ecosystem::Pep440
                .new_range("<2", "3.1.4", Preserve)
                .is_err()
        );
    }
}
