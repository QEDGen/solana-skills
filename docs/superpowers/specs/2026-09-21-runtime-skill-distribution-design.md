# Runtime Skill Distribution Design

**Issue:** [#407](https://github.com/QEDGen/solana-skills/issues/407)

**Date:** 2026-09-21

**Status:** Approved for planning

## Purpose

Make the allowlisted portable QEDGen package the repository's real installation
boundary. A fresh user must continue to run:

```sh
npx skills add qedgen/solana-skills
```

and receive the same `qedgen` and `qedgen-auditor` skill names and runtime
behavior. The installed skills must not contain the Rust workspace, deliberately
vulnerable regression programs, benchmark corpora, example findings, or optional
hook adapters. Those resources remain available to contributors in the source
repository.

Existing users must have an explicit, tested migration path. Skills CLI 1.7.0+
is the supported automatic update path. Older installations can either upgrade
the Skills CLI or explicitly re-add the skill. Because an update replaces the
installed skill directory, users rerun `install.sh` once to restore the local,
checksum-verified QEDGen executable.

## Why the Fixtures Remain

The fixture that prompted the malware classification is not runtime skill data.
`crates/qedgen/tests/fixtures/regressions/v2.21-crucible-crash-first/buggy_anchor`
is an intentionally vulnerable Anchor program consumed by
`crucible_brownfield_smoke.rs`. It proves that brownfield Crucible generation
discovers handlers, creates the program-owned PDA topology, installs the wallet
inflation invariant, and reports the authorization-free drain. The fixture and
its assertions remain unchanged in the development tree and outside both
installable skill directories.

This boundary limits what the Skills CLI copies. It does not assert that a hosted
provider scans only installed files; provider provenance, reclassification, and
fresh badges remain tracked by #412.

## Chosen Architecture

### Repository layout

The root `SKILL.md` moves to `skills/qedgen/SKILL.md`. The QEDGen runtime files
selected by `scripts/skill-distribution.json` are materialized beside it, so the
directory recursively copied by the Skills CLI is exactly the reviewed runtime
package. `package.json` and public documentation point to the new entry point.

The repository continues to hold the canonical development documentation and
source files used to produce the runtime package. Generated copies under
`skills/qedgen/` are committed so GitHub installation needs no build step. The
skill entry point itself is canonical at its new path; it is not duplicated under
another discoverable `SKILL.md` name.

`skills/qedgen-auditor/` is reduced to its runtime inventory. Required schema
validator self-test data remains because the installed validator executes it.
Optional hook adapters and development-only auditor resources move to explicit
non-skill locations, with links for contributors and existing hook users. No
discoverable duplicate skill definition is introduced.

### Deterministic materialization

`scripts/package-skills.py` retains external staging and gains explicit sync and
check operations:

- external staging creates a new portable package for isolated tests;
- sync materializes the committed runtime skill directories from the allowlist;
- check builds the expected package in a temporary directory and fails on any
  missing, changed, or extra tracked runtime file.

All modes continue to validate safe paths, version agreement, frontmatter names,
relative Markdown links, file modes, and SHA-256 inventory entries. Sync stages
the complete result before replacing generated content so a failed build cannot
leave a partial public skill. It never changes the development fixtures.

CI runs the drift check before distribution tests. Reviewers can therefore see
the exact bytes users install, while the allowlist remains the definition of what
may cross the runtime boundary.

## Installation and Migration Behavior

### Fresh installation

For Skills CLI 1.7.0 and newer, the existing repository command discovers
`skills/qedgen/SKILL.md` and `skills/qedgen-auditor/SKILL.md`. Installing either
skill copies exactly its allowlisted directory. The QEDGen wrapper remains
side-effect free: if `bin/qedgen` is absent it prints the explicit `install.sh`
command and exits 127.

The executable is not stored in Git or in the portable package. `install.sh`
reads the packaged `VERSION`, downloads the matching platform release and
checksum, validates both checksum and reported version, and atomically installs
the executable at `bin/qedgen`. The executable continues to embed code-generation
templates, Lean support, and the Kani prelude, so no Rust source tree is needed.

### Existing Skills CLI 1.7.0+ installation

`skills update` follows the skill named `qedgen` from the old root entry point to
`skills/qedgen/SKILL.md`, updates the lockfile path, and keeps the installed skill
name and destination. Replacement removes the old full-repository copy and any
locally downloaded `bin/qedgen`. The wrapper then gives the one-time reinstall
command; rerunning the installer restores the pinned executable.

### Existing Skills CLI 1.5.9 installation

The older updater treats the removed root definition as deleted. Documentation
must not imply that its ordinary update succeeds. The supported paths are:

1. upgrade Skills CLI to 1.7.0 or newer, then update; or
2. explicitly re-add `qedgen` from the unchanged repository command, accepting
   normal replacement of the installed directory.

Both paths end with the same explicit `install.sh` step. Tests preserve the
1.5.9 failure reproduction and prove that explicit re-add installs the new exact
inventory without stale fixture content.

### Optional auditor hook

The hook is not part of the installed runtime skill. Documentation identifies
its new contributor/integration location and warns users whose agent settings
refer to the old installed path to relocate or disable that reference before
updating. The migration does not modify agent settings automatically.

## Test Strategy

Tests exercise observable installation behavior rather than only manifest text:

1. **Inventory:** staged and committed skill trees match byte-for-byte, include
   every required link and executable mode, and exclude development fixtures,
   hooks, benchmarks, examples, build trees, and source crates.
2. **Fresh install:** Skills CLI 1.7.0 discovers the same skill names, installs
   exact inventories, and can independently install QEDGen and the auditor.
3. **Modern migration:** a disposable Git repository moves from the legacy root
   layout to the runtime subdirectory; update changes the lock path, removes
   stale development content and the old local executable, and exposes the
   explicit reinstall instruction.
4. **Legacy migration:** Skills CLI 1.5.9 reproduces the deleted-path behavior,
   then explicit re-add succeeds with the new inventory.
5. **Installer and wrapper:** hermetic fake-release tests cover missing tools,
   checksums, version validation, replacement preservation, and reinstall after
   migration without writing to a real home directory.
6. **Representative runtime:** an isolated installed package runs help/version,
   validates a spec, and emits Anchor, Lean, and Kani artifacts using a real
   release-equivalent QEDGen binary.
7. **Development regression:** existing Crucible and auditor tests continue to
   use source-tree fixtures from their unchanged development locations.

The full existing `npm test` suite remains the final local gate. The pinned Skills
CLI compatibility matrix remains hermetic and project-local.

## Documentation and Release Contract

Distribution documentation changes from "groundwork" to the active public
layout. It explains the minimum automatic-update version, the 1.5.9 re-add
fallback, one-time QEDGen reinstall, optional-hook migration, exact inventory,
and limits of provider scan claims. Release instructions require sync followed
by drift checking whenever an allowlisted source changes.

No GitHub release, external repository, provider appeal, or skills.sh rescan is
performed as part of #407. Those are separate release and #412 actions.

## Alternatives Rejected

- **Move every canonical runtime reference into the skill directory.** This
  avoids generated copies but creates broad path churn across developer tooling
  and historical documentation without improving the installation boundary.
- **Publish a separate generated repository.** This gives a strong physical
  boundary but changes the established install source.
- **Keep the root entry point.** Skills CLI recursively copies its containing
  directory, so the Rust workspace and vulnerable development fixtures remain
  part of every QEDGen installation.

## Success Criteria

- The unchanged install command produces only the reviewed runtime inventories.
- New users retain the same skill names, explicit CLI installation flow, and
  generated QEDGen behavior.
- Existing 1.7.0+ and 1.5.9 users have tested, accurate migration instructions.
- Deliberately vulnerable fixtures remain effective development tests and never
  appear in an installed skill.
- CI detects any drift between the allowlist, committed package, and tested
  installation.
