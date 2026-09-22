# Portable skill distribution

The repository and published installation command remain unchanged:

```sh
npx skills add qedgen/solana-skills
```

The public command, repository, and skill names did not change when the runtime
definitions moved under `skills/`. Fresh installs continue to discover
`qedgen`, `qedgen-auditor`, and the development benchmark skill. Installing a
runtime skill now copies only that skill's allowlisted runtime files.

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

## Existing-install migration

Skills CLI 1.7.0 follows the skill name across the root-to-subdirectory move
during an ordinary project update. Skills CLI 1.5.9 does not: upgrade the client
or explicitly re-add `qedgen` from the same repository, agent, and installation
scope. The tested recovery command is:

```sh
npx skills add qedgen/solana-skills --skill qedgen
```

Replacement prunes the old development content and local binary. Rerun
`bash install.sh` from the installed `qedgen` directory afterward. The wrapper
does not search `PATH`, download, or build. The exact user procedure is in
[installation and prerequisites](../references/installation.md).

The optional Claude Code auditor hook moved out of the installed auditor skill
to `integrations/qedgen-auditor-hooks/`. It is never packaged or automatically
enabled. Users who previously enabled the old hook should remove that settings
entry or manually relocate the adapter into stable user-owned storage before
replacing the skill.

Hosted providers may scan a selected skill directory or the entire repository;
the local CLI tests do not establish their scope or force a production listing
rescan. Track provider classification and rescanning separately in
[#412](https://github.com/QEDGen/solana-skills/issues/412).

## Source synchronization

`references/`, `templates/`, `install.sh`, `tools/qedgen`, and other allowlisted
root files remain canonical development sources. After changing one, regenerate
and verify the committed runtime copy:

```sh
python3 scripts/package-skills.py --sync
python3 scripts/package-skills.py --check
```

The check fails on content, executable-bit, inventory, version, or link drift.
CLI setup remains explicit: the installer verifies and stages a replacement
before changing the existing binary, and PATH links require `--link-dir`.
