# PR draft — audit metadata passthrough

Ready-to-paste title and body for the PR from
`ForrestThump:feat/audit-metadata-passthrough` → `Epistates:main`.

Open at: https://github.com/ForrestThump/turbovault/pull/new/feat/audit-metadata-passthrough
(set base to `Epistates:main`).

---

## Title

```
feat(vault): allow attaching custom metadata to audit log entries
```

## Body

```markdown
## Summary

`AuditEntry` already carries a free-form `metadata: serde_json::Value` field, but
the `VaultManager` write APIs construct the audit entry internally and give callers
no way to populate it. So an SDK consumer can read rich audit history but can't
*write* anything to it — there's no way to attribute a change at the point it's made
(e.g. record who/what performed the write, a correlation id, or a reason), even
though the storage for it already exists and is serialized.

This PR plumbs that existing field through the four mutating operations.

## What changed

New `*_with_metadata` variants on `VaultManager`:

- `write_file_with_metadata`
- `edit_file_with_metadata`
- `delete_file_with_metadata`
- `move_file_with_metadata`

Each takes an `Option<serde_json::Value>` and forwards it onto the recorded
`AuditEntry.metadata`. The existing methods become thin wrappers that pass `None`.

`FileTools` gains matching wrappers, following the existing `*_with_hash` convention
(`write_file_with_mode_and_metadata`, `edit_file_with_metadata`,
`delete_file_with_metadata`, `move_file_with_metadata`).

## Properties

- **Fully backward compatible.** No existing signature or call site changes; the
  current methods delegate with `None`. A purely additive surface.
- **Vendor-neutral.** Core takes no opinion on the metadata's shape — it's stored
  verbatim. Write provenance, correlation ids, and actor names are just example
  uses.
- **No behavioral change to writes.** Metadata is recorded only when an audit log is
  configured, and never affects the bytes written to disk. For `edit_file`, a
  `dry_run` (which writes nothing) records nothing.

## Intentional scope boundary

The MCP tool JSON schemas are deliberately left unchanged. Whether to let
*over-the-wire* callers supply audit metadata is a separate decision (it has
trust/spoofing implications that the SDK surface does not), so it's left for a
follow-up rather than bundled here. This PR is SDK-only.

## Tests

Added round-trip tests for write / edit / delete / move asserting the metadata
lands on the recorded audit entry, plus the default-empty-object case when no
metadata is supplied. Full `turbovault-vault` and `turbovault-tools` suites pass;
`cargo fmt --check` and `cargo clippy` are clean on the changed code.
```

---

## Notes for the author (not part of the PR body)

- If the concurrency-hardening PR (#31) merges first, this branch will conflict in
  `edit_file`. Resolution: keep #31's `validated_hash`, and the relocated write call
  inside `edit_file_with_metadata` becomes
  `self.write_file_with_metadata(&vault_path, &new_content, Some(&validated_hash), metadata).await?;`
- Branch is based on upstream `main` (`5d6bdd9`); rebase onto latest `upstream/main`
  before opening if upstream has moved.
