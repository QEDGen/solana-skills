# Portable skill distribution

The repository and published installation command remain unchanged:

```sh
npx skills add qedgen/solana-skills
```

This is packaging groundwork for [#407](https://github.com/QEDGen/solana-skills/issues/407).
It does not move root `SKILL.md`, publish an artifact, or change existing users'
install/update paths. No new repository is needed for the builder.

## Build and inspect

Requires Python 3.9+ and Git. Use a new output directory outside the checkout:

```sh
python3 scripts/package-skills.py --output /tmp/qedgen-portable
python3 -m unittest discover -s scripts/tests -p test_skill_distribution.py
```

`scripts/skill-distribution.json` is the explicit file allowlist. Keys are
package-relative destinations; values are canonical repository sources. The
builder does not infer exclusions from `.gitignore`, npm metadata, or undocumented
`.skillignore` behavior. Invalid paths, missing sources, version drift, and broken
local Markdown links fail the build. Existing output directories are refused,
and a failed build does not leave partially staged output at the requested path.

The output contains independent `skills/qedgen/` and `skills/qedgen-auditor/`
directories plus `distribution.json`. The inventory records the version, source
commit, manifest hash, and each staged file's SHA-256. Hashes describe the actual
staged bytes, which may include uncommitted edits; the commit alone is not a
clean-release attestation.

Included resources:

- QEDGen instructions and references, selected supporting documentation, the
  installer, wrapper, generated `VERSION`, and license.
- Auditor instructions, references, scripts, schemas, generated `VERSION`, and
  license. Small JSON validator self-test fixtures remain bundled because
  `check-domain-artifacts.sh` uses them. Source-only review remains available
  without QEDGen.
- The finding-to-spec mapping in both skills for independent use.

Vulnerable program regression fixtures, benchmark corpora, example findings, optional Claude hooks,
and source/build trees are excluded. Those resources remain intact in the source
repository. The benchmark skill remains available through existing repository
installation; it is deliberately not a portable runtime package.

The release binary embeds codegen templates, Lean support, and the Kani prelude.
An isolated real-binary smoke test exercises their emission. The portable
installer reads generated `VERSION` and downloads the matching checksum-verified
release. If unavailable, it explains that a source checkout is required to build
locally. Full checkouts still derive the version from `crates/qedgen/Cargo.toml`
and retain the Cargo fallback.

## Compatibility evidence

Investigation on 2026-09-19 found that the skills CLI recursively copies the
selected `SKILL.md`'s containing directory. A root definition includes development
resources; the ordinary copy filter does not implement our allowlist. See upstream
[discovery](https://github.com/vercel-labs/skills/blob/7407f3893ad4dceab546ac002c3ef806e4000c73/src/skills.ts) and
[installation](https://github.com/vercel-labs/skills/blob/7407f3893ad4dceab546ac002c3ef806e4000c73/src/installer.ts).

| CLI | Package discovery/install and reinstall cleanup | Root-to-subdirectory project update |
|---|---|---|
| 1.5.9 | Pass | Treats the old path as deleted; skips the skill |
| 1.7.0 | Pass | Resolves the skill by name and records its new path |

The update test uses disposable local Git repositories and real CLI subprocesses.
It neither publishes to GitHub nor contacts the production listing. These results
cover project updates, not all historical versions, global lockfile formats, or
provider scanner behavior.

Run with an installed CLI and supported runtime (1.7.0 needs Node >=22.20.0):

```sh
python3 scripts/test-skills-cli.py --cli /path/to/skills/bin/cli.mjs --migration
# For 1.5.9, reproduce the known blocker:
python3 scripts/test-skills-cli.py --cli /path/to/skills/bin/cli.mjs \
  --migration --expect-migration-blocked
# Also exercise templates embedded in a matching QEDGen binary:
python3 scripts/test-skills-cli.py --cli /path/to/skills/bin/cli.mjs \
  --qedgen /absolute/path/to/qedgen
```

`--runtime` selects a non-default runtime path. Tests isolate homes and projects,
disable telemetry, and never install into the developer's agent directories.
The QEDGen smoke validates and generates artifacts; it does not claim generated
programs or proofs compile or pass verification. CI runs package tests and both
CLI cases, installing pinned CLI dependencies with lifecycle scripts disabled.

## Rollout gate

Preserve the root entry point while compatibility with older users is required.
Do not commit another discoverable definition with the same `qedgen` name: it
would make discovery ambiguous and still leave the root copying the repository.
The builder refuses to stage inside the checkout for the same reason.

A future rollout can publish portable artifacts or move the definition only after
a supported-client/migration policy is agreed and relevant project/global update
paths are verified. Preserve repository identity and skill names. An explicit
re-add by name is a tested replacement route, but requiring it is still disruption.

Hosted providers may scan a selected skill directory or the entire repository;
these tests do not establish their scope. Until public distribution changes or
the provider corrects the classification, existing installs and the listing still
use the root package. Track the appeal/rescan separately in
[#412](https://github.com/QEDGen/solana-skills/issues/412).
