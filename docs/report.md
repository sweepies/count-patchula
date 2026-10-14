# Batch report contract (version 1)

`rustovate-report` is the shared caller interface for the future `lookup --json`
and `run --json` results, and for CLI/server callers of the engine. Phase 0 only
defines the contract; lookup/run still fail explicitly as unimplemented. The
existing config-check JSON is a different command result, not a batch report.

Deserialize untrusted input with `Report::from_json`, which checks `schemaVersion`
before interpreting the payload and validates identities. Unknown fields fail.
A future incompatible schema increments the version; consumers must reject it
rather than treating a new payload as an empty successful lookup.

```json
{
  "schemaVersion": 1,
  "repositories": [{
    "repository": "owner/repo",
    "status": "complete",
    "reason": null,
    "packageFiles": [{
      "path": "Cargo.toml",
      "dependencies": [{
        "manager": "cargo",
        "name": "serde",
        "package": "serde",
        "current": {"value": "1", "version": "1.0.0", "digest": null},
        "updates": [{
          "newValue": "1", "newVersion": "1.1.0", "newDigest": null,
          "updateType": "minor", "group": null
        }]
      }]
    }]
  }]
}
```

All collection fields are required, including empty collections. Optional string
fields accept null or omission; producers should emit null consistently. Current
value means manifest text, version means resolved/extracted version, and digest
means the hash itself. Unknown versions stay null; consumers must not infer a
version from a range. `package` is the registry identity (for aliases it can differ
from `name`). Manager names use rustovate's configuration vocabulary. Update types
are `major`, `minor`, `patch`, `digest`, `pin`; comparison output also preserves
other upstream update types so unsupported maintenance is visible. `group` is
the human group identity, not a tool-specific branch slug. Update lists are sets,
not ordered preference lists; each proposal must have at least one target field.

Repository status is `complete`, `skipped`, or `error`. Skipped/error repositories
require a reason. An empty complete repository means a successful extraction with
no supported dependencies. A caller must never convert errors into that state.

Repository and file paths are unique. Within a file, `(manager, name)` identifies
a dependency; producers merge identical occurrences and their distinct proposals.
Ambiguous occurrences with different packages or current fields are rejected in
v1, rather than silently losing a dependency. Supporting distinct occurrences
will need an explicit extension before managers emit such reports.

The hand-written [sample](../conformance/tests/fixtures/report.json) exercises
aliases, ranges, grouping and digest policy. It is not a real lookup result.
