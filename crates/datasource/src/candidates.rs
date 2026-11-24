//! Which listed versions a lookup must describe: the versions policy can
//! read a date or flag of. Sources that date one version per request (Go's
//! `.info`, a Maven POM, a NuGet registration page) ask only about these;
//! every other version is listed unchecked. A release newer than every
//! dependency's baseline is always among them, wherever it sorts, so a patch
//! release on an older line is dated like a new major.
use patchula_versioning::{Ecosystem, Parsed};
use patchula_words::{Scheme, Source};
use serde::{Deserialize, Serialize};

/// The versions a lookup describes. The default is every version.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Candidates(Option<Floor>);

/// Versions newer than `after` in the order of the scheme `scheme` and
/// `source` select, prereleases among them only when `prereleases` is set.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Floor {
    scheme: Scheme,
    source: Source,
    after: String,
    prereleases: bool,
}
impl Floor {
    fn ecosystem(&self) -> Ecosystem {
        Ecosystem::of(self.scheme, self.source)
    }
}

impl Candidates {
    /// Every listed version.
    pub fn all() -> Self {
        Self(None)
    }
    /// Versions newer than `after` in the order of the scheme `scheme` and
    /// `source` select, prereleases only when `prereleases` is set. A floor
    /// that scheme cannot parse is every version.
    pub fn after(scheme: Scheme, source: Source, after: &str, prereleases: bool) -> Self {
        if Ecosystem::of(scheme, source).parse(after).is_err() {
            return Self::all();
        }
        Self(Some(Floor {
            scheme,
            source,
            after: after.to_owned(),
            prereleases,
        }))
    }
    /// Whether this is every version.
    pub fn is_all(&self) -> bool {
        self.0.is_none()
    }
    /// Whether `version` is among the versions described.
    pub fn describes(&self, version: &str) -> bool {
        self.wanted().contains(version)
    }
    /// The floor, parsed once, for testing many versions against it.
    pub(crate) fn wanted(&self) -> Wanted {
        Wanted(self.0.as_ref().and_then(|floor| {
            let scheme = floor.ecosystem();
            Some((scheme, scheme.parse(&floor.after).ok()?, floor.prereleases))
        }))
    }
    /// Widen to also cover `other`: the lower floor, prereleases when either
    /// wants them, and every version when either does or their schemes
    /// differ.
    pub fn widen(&mut self, other: &Self) {
        let (Some(mine), Some(theirs)) = (&mut self.0, &other.0) else {
            self.0 = None;
            return;
        };
        let scheme = mine.ecosystem();
        if scheme != theirs.ecosystem() {
            self.0 = None;
            return;
        }
        let (Ok(a), Ok(b)) = (scheme.parse(&mine.after), scheme.parse(&theirs.after)) else {
            self.0 = None;
            return;
        };
        if scheme.order(&b, &a).is_lt() {
            mine.after.clone_from(&theirs.after);
        }
        mine.prereleases |= theirs.prereleases;
    }
    /// Whether every version `other` wants is one this wants too, so a
    /// catalog described for this serves a lookup for `other`.
    pub fn covers(&self, other: &Self) -> bool {
        let mut widened = self.clone();
        widened.widen(other);
        widened == *self
    }
}

/// A parsed floor: which version texts a lookup describes.
pub(crate) struct Wanted(Option<(Ecosystem, Parsed, bool)>);
impl Wanted {
    /// Whether `version` is described. A version the floor's scheme cannot
    /// parse is described, since nothing proves policy will not read it.
    pub(crate) fn contains(&self, version: &str) -> bool {
        let Some((scheme, floor, prereleases)) = &self.0 else {
            return true;
        };
        let Ok(parsed) = scheme.parse(version) else {
            return true;
        };
        scheme.order(&parsed, floor).is_gt() && (*prereleases || parsed.0.pre.is_empty())
    }
    /// Whether some version newer than `upper` may be described: `false`
    /// only when `upper` is at or below the floor.
    pub(crate) fn reaches_above(&self, upper: &str) -> bool {
        let Some((scheme, floor, _)) = &self.0 else {
            return true;
        };
        scheme
            .parse(upper)
            .map_or(true, |upper| scheme.order(&upper, floor).is_gt())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A floor describes newer releases only; prereleases only on request;
    /// a version its scheme cannot parse is described anyway.
    #[test]
    fn a_floor_describes_newer_releases_wherever_they_sort() {
        let wanted = Candidates::after(Scheme::Go, Source::Go, "v1.2.3", false).wanted();
        // A patch release on the current line, and a new major.
        assert!(wanted.contains("v1.2.4"));
        assert!(wanted.contains("v2.0.0"));
        assert!(!wanted.contains("v1.2.3"));
        assert!(!wanted.contains("v1.1.9"));
        assert!(!wanted.contains("v1.3.0-rc.1"));
        assert!(wanted.contains("not-a-version"));
        let pre = Candidates::after(Scheme::Go, Source::Go, "v1.2.3", true).wanted();
        assert!(pre.contains("v1.3.0-rc.1"));
        assert!(Candidates::all().wanted().contains("v0.0.1"));
        // A floor its scheme cannot read is every version.
        assert!(Candidates::after(Scheme::Go, Source::Go, "latest", false).is_all());
    }

    /// Widening keeps the lower floor and any prerelease request; a view
    /// described for a wider set covers a narrower one, never the reverse.
    #[test]
    fn widening_and_covering_follow_the_lowest_floor() {
        let low = Candidates::after(Scheme::Maven, Source::Maven, "1.2.3", false);
        let high = Candidates::after(Scheme::Maven, Source::Maven, "1.5", true);
        let mut both = high.clone();
        both.widen(&low);
        assert_eq!(
            both,
            Candidates::after(Scheme::Maven, Source::Maven, "1.2.3", true)
        );
        assert!(both.covers(&low) && both.covers(&high));
        assert!(!high.covers(&low));
        assert!(!low.covers(&high), "low has no prereleases");
        assert!(Candidates::all().covers(&low));
        assert!(!low.covers(&Candidates::all()));
        let mut mixed = low.clone();
        mixed.widen(&Candidates::after(Scheme::Go, Source::Go, "v1.0.0", false));
        assert!(mixed.is_all(), "schemes that differ describe every version");
    }
}
