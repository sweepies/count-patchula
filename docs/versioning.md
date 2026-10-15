# Versioning

SWE-96 uses `nodejs-semver` for npm and `semver` for Cargo. Their requirements
are intentionally separate: bare `1.2.3` is exact in npm, compatible in Cargo.
Pre-release matching follows the upstream parsers; precedence ignores metadata.
Numeric major/minor/patch classification follows npm's SemVer components. Cargo
classifies a changed leftmost nonzero component as major, including 0.x minors
and 0.0.x patches. Go accepts canonical v-prefixed SemVer and pseudo-versions;
loose accepts numeric dotted versions with an optional v prefix.

Exact uses `=` in Cargo (a bare version would allow compatible releases).
Preserve keeps already-satisfied ranges and shifts simple caret, tilde and
wildcard ranges. Compound preserve edits fail explicitly rather than guessing.
Widen uses a union for npm and a convex compatible upper interval for Cargo,
retaining the lower minimum.

Docker parses numeric version prefixes (including more than three components)
and preserves the complete suffix variant. Alpine/bookworm and other variants
never cross during policy evaluation; suffixes are compatibility labels rather
than SemVer prereleases. Floating/opaque image tags receive digest updates.
PEP 440 uses `pep440_rs` for epochs, arbitrary release components, pre/dev/post
and local ordering, normalization, wildcard/exclusion/compatible specifiers.
Epoch changes classify as major; pre/dev releases need prerelease opt-in.
Exact Python requirements use `==`; existing satisfied preserve/widen constraints
stay intact. HashiCorp implements equality, inequality, conjunctions and `~>`
pessimistic upper bounds (two-component and three-component forms differ).
Compound preserve edits that would require guessing still fail explicitly;
the default exact policy handles them without rewriting an interval.

Independently written test references:
[npm ranges](https://docs.npmjs.com/cli/v6/using-npm/semver/),
[Cargo requirements](https://doc.rust-lang.org/cargo/reference/specifying-dependencies.html),
[Go versions](https://go.dev/ref/mod#versions),
[PEP 440](https://packaging.python.org/en/latest/specifications/version-specifiers/),
[HashiCorp constraints](https://developer.hashicorp.com/terraform/language/expressions/version-constraints).
