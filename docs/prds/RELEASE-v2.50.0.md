# QEDGen v2.50.0: an explicit, auditable runtime distribution

**Scope:** 10 merged PRs since v2.49.0 (#396, #397, #403, #413, #414,
#416, #417, #418, #419, and #420), closing #395, #398, #399, #400, #401,
#402, #407, #408, #409, #410, and #411.

**Theme:** the installed skills are now a defined product boundary rather than
an incidental copy of the development repository. The release also hardens the
CLI installation and feedback paths, strengthens auditor evidence, expands
upgrade-safety checks, and validates the unsafe boundary in generated
Pinocchio proofs.

## 1. Portable skills have an explicit distribution boundary (#407)

The public runtime skills now live under `skills/qedgen/` and
`skills/qedgen-auditor/`. An allowlisted package manifest defines every shipped
file, and the package builder validates paths, local Markdown links, versions,
file modes, and content hashes.

The portable package excludes vulnerable regression fixtures, benchmark
corpora, source/build trees, and optional editor hooks. Those resources remain
available in the development repository but are not runtime dependencies of
either installed skill. Hermetic tests exercise package creation, discovery,
installation, replacement, rollback, and migration with Skills CLI 1.5.9 and
1.7.0.

The public repository, skill names, and install command remain unchanged:

```sh
npx skills add qedgen/solana-skills
```

Skills CLI 1.7.0 follows the skill name across the root-to-subdirectory move.
With 1.5.9, explicitly re-add the skill after updating:

```sh
npx skills add qedgen/solana-skills --skill qedgen
```

## 2. CLI installation is explicit and failure-safe (#408)

The skill wrapper no longer installs software or creates PATH links as a side
effect of discovering a missing CLI. Installation is an explicit operation;
PATH links require `--link-dir`, and source builds require `--from-source`.

Release downloads remain checksum-verified. A candidate binary is staged,
validated, and version-checked before atomic activation, so integrity,
execution, or version failures preserve the known-working binary. Lean and
Kani prerequisite checks inspect PATH without invoking toolchain-manager
shims or triggering downloads.

## 3. Trust boundaries and feedback privacy are documented and enforced

Both skills treat source comments, specs, IDL fields, generated files,
diagnostics, and tool output as untrusted task data (#409). Documentation now
states the data flow for feedback submission and the separately installed,
optional Claude hook.

The feedback workflow now redacts common credential forms before persistence
or submission, omits absolute working-directory metadata, and limits spec paths
to file names (#410). The user-edited Markdown draft is reloaded and sanitized
before either `gh` submission or URL fallback. URL fallback observes a complete
8,000-byte encoded budget and retains a truncation notice when space permits.

## 4. Auditor evidence and benchmark contracts are stricter

The auditor adds a known-non-findings reference for seven commonly misreported
Solana patterns and four new catalog categories (#396). Rejected findings must
name the concrete runtime or framework guarantee; a generic reference to the
document is not sufficient evidence.

Readiness checks now reconcile source handlers with explicit IDLs, including
empty IDLs and multi-program workspaces. Probe category identity has one
canonical source, generated Anchor `--all` artifacts are compile-checked, and
benchmark manifests and reports are schema-validated with tier-preserving
scoring (#403). Auditor knowledge-base entries now carry checkable provenance;
legacy prose-only entries are explicitly allowlisted so new unsourced entries
cannot silently enter the catalog.

## 5. Upgrade checks use Ratchet 0.4.0

Ratchet 0.4.0 fixes exact-length Quasar discriminator comparison and adds five
rules through the existing `check` and `preflight` surfaces (#397):

- R017: event removed
- R018: error code changed
- R019: nested type shape changed
- R020: serialization changed
- P007: discriminator prefix collision

Legacy pre-0.30 Anchor IDLs now fail with a conversion hint instead of
normalizing to an empty surface that cannot exercise layout rules.

## 6. Generated Pinocchio proofs use the audited account layout (#411)

Synthetic Pinocchio `AccountInfo` values now start with the actual clear borrow
state. The stack allocation reserves Pinocchio's full 10,240-byte permitted
reallocation region, and the raw pointer is derived from the complete backing
allocation so its provenance covers the header, data, and growth area.

Compile-time assertions cover size, alignment, field offsets, and contiguity.
Behavioral tests exercise real data and lamport borrow tracking, token and mint
parsing, zero-length data, and both sides of the realloc boundary. Generated
crates pin one audited dependency family. Repository compile-smoke tests inspect
Cargo metadata and reject generated graphs containing any Pinocchio version
other than 0.8.4.

## 7. CI and dependency drift

- `rustls` is updated to the patched 0.23.45 line.
- Runtime SBF journeys serialize first-use platform-tool installation.
- Parallax pin checks distinguish a genuine 404 from authentication, rate-limit,
  and network failures while still reporting staleness.

## Provider classification

This release materially narrows the portable runtime distribution and removes
regression fixtures from installed skill payloads. It does not claim that a
hosted provider scans only that boundary or that an existing classification
will refresh automatically. After publication, #412 tracks the exact artifact
revision, report date, and fresh skills.sh/Socket result. An appeal remains a
follow-up only if the fixture classification persists.

## Upgrading

No `.qedspec` or CLI flag migration is required. Regenerate Pinocchio Kani
harnesses to pick up the corrected account construction. Existing skill
installations should update with Skills CLI 1.7.0 or newer; 1.5.9 users should
use the explicit re-add command above, then run the skill's installer to fetch
the v2.50.0 CLI binary.
