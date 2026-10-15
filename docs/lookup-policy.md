# Lookup policy (SWE-101)

Policy starts at explicit defaults and applies matching rules in declaration
order, independently for each update type. Populated selector fields are AND;
patterns within one field are OR, with exclusions winning. Lists replace lists.
An empty group removes grouping. ignore and exclude suppress proposals.

The newest permitted, nondeprecated, nonyanked version is proposed per numeric
update class. Prereleases require a prerelease baseline or an explicit permitted
filter, allowing the remote-agent nightly rule. Resolved lock versions provide
the baseline where available; npm ranges use their mathematical lower boundary
only for classification, never as a fabricated resolved report version.

Actions retain opaque branch refs. Their digest is resolved for that same ref;
the ops-workflows rule disables digest changes. Version tags can upgrade and be
pinned to their commit. SHA pins without a version comment cannot safely select
a replacement tag and remain unchanged with a warning from the engine.

The v1 update has optional `policy` metadata containing waitHours, windows,
merge, labels, commitPrefix, releaseNotes and bodyNotes. Timing constraints are
reported, not executed by lookup. Existing v1 payloads deserialize without this
metadata; the conformance comparison continues comparing its original fields.

Tests use fixed catalogs with the renovate-runner, fnox and remote-agent example
configs, plus ordered filters, exclusions, empty-list overrides and windows.
