# Runtime Skill Distribution Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make `skills/qedgen/` and `skills/qedgen-auditor/` the exact public installation boundary while preserving the existing repository command and tested migration paths.

**Architecture:** Keep `scripts/skill-distribution.json` as the allowlist and refactor `package-skills.py` so one staged tree drives external packages, committed-tree synchronization, and CI drift checks. Move the root QEDGen entry point into its public skill directory, relocate auditor development helpers outside the public directory, and exercise the real repository layout with pinned Skills CLI versions.

**Tech Stack:** Python 3.9+ (`unittest`, `pathlib`, `tempfile`, `hashlib`), Bash, Git, Node.js 22, Skills CLI 1.5.9/1.7.0, Rust/QEDGen.

**Spec:** `docs/superpowers/specs/2026-09-21-runtime-skill-distribution-design.md`

## Global Constraints

- Preserve `npx skills add qedgen/solana-skills` and the names `qedgen` and `qedgen-auditor`.
- Skills CLI 1.7.0+ is the supported automatic root-to-subdirectory update path.
- Skills CLI 1.5.9 gets a tested upgrade-or-explicit-re-add path.
- The wrapper stays side-effect free; installation remains explicit via `bash install.sh`.
- Do not store the QEDGen executable in Git. After replacement removes `bin/qedgen`, the wrapper must explain the reinstall command and exit 127.
- Keep vulnerable regression programs, benchmarks, example findings, source/build trees, and optional hooks outside installable skill directories.
- Keep the auditor schema-validator samples used by `check-domain-artifacts.sh` inside the auditor skill.
- Do not modify agent settings, publish a release, create a repository, submit an appeal, or request a rescan.

## Review Focus

- An ignored local `skills/qedgen/bin/qedgen` must not count as committed drift or enter a staged package; Task 1 tests it.
- A failed sync after staging must restore both prior public directories rather than update only one; Task 1 tests rollback.
- Skills CLI 1.5.9 explicit re-add must replace a stale root-layout installation after ordinary update reports it deleted; Task 2 tests it.
- A 1.7.0 migration must remove the old local binary and give an exact reinstall instruction rather than use PATH; Task 2 tests it.
- Existing optional-hook settings may point to the removed installed path; Task 1 preserves the adapter elsewhere and Task 3 documents manual migration.

---

### Task 1: Deterministic committed runtime trees

**Files:**
- Modify: `scripts/tests/test_skill_distribution.py`
- Modify: `scripts/package-skills.py`
- Modify: `scripts/skill-distribution.json`
- Move: `SKILL.md` → `skills/qedgen/SKILL.md`
- Generate: `skills/qedgen/**`
- Add: `skills/qedgen-auditor/LICENSE`
- Move: `skills/qedgen-auditor/hooks/**` → `integrations/qedgen-auditor-hooks/**`
- Move: `skills/qedgen-auditor/scripts/check-knowledge-bases.sh` → `scripts/check-auditor-knowledge-bases.sh`
- Move: `skills/qedgen-auditor/references/basis-corpus-registry.txt` → `scripts/data/auditor-basis-corpus-registry.txt`
- Move: `skills/qedgen-auditor/references/basis-legacy-allowlist.txt` → `scripts/data/auditor-basis-legacy-allowlist.txt`
- Modify: `scripts/check-auditor-skill.sh`
- Modify: `scripts/test-auditor-preflight.sh`
- Modify: `integrations/qedgen-auditor-hooks/README.md`
- Modify: `skills/qedgen-auditor/references/model-selection.md`
- Modify: `skills/qedgen-auditor/references/audit-handbook.md`
- Modify: `skills/qedgen-auditor/references/workflow_walkthrough.md`

**Interfaces:**
- Consumes manifest schema 1, mapping package-relative destinations to repository-relative sources.
- Produces `stage_package(stage: Path) -> dict`, `sync_public_skills() -> None`, and `check_public_skills() -> None`.
- CLI modes `--output PATH`, `--sync`, and `--check` are mutually exclusive.

- [ ] **Step 0: Capture the optional-hook documentation RED baseline**

Give an independent evaluator the currently installed auditor skill and ask:
"I installed `qedgen-auditor` normally. Is its thinking-budget hook present and
enabled, and what must I do to use it?" Record any claim that `hooks/` is bundled
or automatically enabled. Do not disclose the intended answer or edit the
auditor references before recording this baseline.

- [ ] **Step 1: Write failing committed-boundary tests**

Add to `DistributionTests`:

```python
    def run_builder(self, *args, expected=0, env=None):
        result = subprocess.run(
            [sys.executable, str(BUILDER), *map(str, args)],
            capture_output=True, text=True, env=env,
        )
        self.assertEqual(result.returncode, expected, result.stdout + result.stderr)
        return result

    def test_committed_skill_trees_match_staged_inventory(self):
        output = self.build()
        for name in ("qedgen", "qedgen-auditor"):
            expected = output / "skills" / name
            actual = ROOT / "skills" / name
            def snapshot(root, ignore_bin=False):
                return {
                    p.relative_to(root).as_posix(): (p.read_bytes(), p.stat().st_mode & 0o111)
                    for p in root.rglob("*")
                    if p.is_file() and not (ignore_bin and p.relative_to(root).parts[:1] == ("bin",))
                }
            self.assertEqual(snapshot(actual, name == "qedgen"), snapshot(expected))

    def test_root_skill_definition_is_removed(self):
        self.assertFalse((ROOT / "SKILL.md").exists())
        self.assertTrue((ROOT / "skills/qedgen/SKILL.md").is_file())

    def test_check_ignores_local_installed_binary(self):
        local_bin = ROOT / "skills/qedgen/bin/qedgen"
        local_bin.parent.mkdir(parents=True, exist_ok=True)
        local_bin.write_text("local runtime state")
        self.addCleanup(lambda: local_bin.unlink(missing_ok=True))
        self.run_builder("--check")

    def test_auditor_tree_excludes_optional_development_helpers(self):
        auditor = ROOT / "skills/qedgen-auditor"
        self.assertFalse((auditor / "hooks").exists())
        self.assertFalse((auditor / "scripts/check-knowledge-bases.sh").exists())
        self.assertFalse((auditor / "references/basis-corpus-registry.txt").exists())
        self.assertFalse((auditor / "references/basis-legacy-allowlist.txt").exists())
        self.assertTrue((ROOT / "integrations/qedgen-auditor-hooks/auditor-thinking-budget.sh").is_file())
        self.assertTrue((ROOT / "scripts/check-auditor-knowledge-bases.sh").is_file())
```

Add `test_sync_rolls_back_both_public_trees_on_second_replacement_failure`.
It copies the public trees and required source inputs into
`self.scratch / "sync-root"`, invokes the builder with
`["--sync", "--root", str(self.scratch / "sync-root")]` under
`QEDGEN_PACKAGE_TESTING=1` and
`QEDGEN_TEST_SYNC_FAIL_AFTER=qedgen`, and asserts hashes of both original trees
are unchanged. `--root` must be rejected unless `QEDGEN_PACKAGE_TESTING=1`.

- [ ] **Step 2: Verify RED**

```bash
python3 -m unittest \
  scripts.tests.test_skill_distribution.DistributionTests.test_committed_skill_trees_match_staged_inventory \
  scripts.tests.test_skill_distribution.DistributionTests.test_root_skill_definition_is_removed \
  scripts.tests.test_skill_distribution.DistributionTests.test_check_ignores_local_installed_binary \
  scripts.tests.test_skill_distribution.DistributionTests.test_auditor_tree_excludes_optional_development_helpers \
  scripts.tests.test_skill_distribution.DistributionTests.test_sync_rolls_back_both_public_trees_on_second_replacement_failure
```

Expected: failures because `skills/qedgen`, `--check`, `--sync`, rollback, and
the separated auditor development paths do not exist.

- [ ] **Step 3: Refactor the builder around one staged tree**

Retain all existing path/version/frontmatter/link/hash validation and introduce:

```python
PUBLIC_SKILL_NAMES = ("qedgen", "qedgen-auditor")

def file_snapshot(root, ignore_runtime_bin=False):
    result = {}
    for path in sorted(root.rglob("*")):
        if not path.is_file():
            continue
        relative = path.relative_to(root)
        if ignore_runtime_bin and relative.parts[:1] == ("bin",):
            continue
        result[relative.as_posix()] = (
            hashlib.sha256(path.read_bytes()).hexdigest(),
            path.stat().st_mode & 0o111,
        )
    return result

def check_public_skills(root=ROOT):
    with tempfile.TemporaryDirectory(prefix="qedgen-public-check-") as scratch:
        stage = Path(scratch) / "package"
        stage.mkdir()
        stage_package(stage)
        for name in PUBLIC_SKILL_NAMES:
            expected = file_snapshot(stage / "skills" / name)
            actual = file_snapshot(root / "skills" / name, name == "qedgen")
            if actual != expected:
                raise ValueError(f"committed public skill drift: {name}; run --sync")
```

`sync_public_skills()` must stage everything first, rename both old directories
to backups, install both replacements, and restore both backups on any exception.
Ignore/preserve only `skills/qedgen/bin/` as local runtime state. Test-only fault
injection is active only when `QEDGEN_PACKAGE_TESTING=1`.

Rename the current `build(output)` copy/validation body to
`stage_package(stage)`: it reads the same manifest, copies each destination,
generates `VERSION`, validates frontmatter and relative links, calculates the
same inventory fields, writes `distribution.json`, and returns the inventory.
The `--output` caller retains existing-destination and outside-repository checks,
creates a temporary stage, invokes `stage_package`, and renames the stage to the
requested output.

Use:

```python
mode = parser.add_mutually_exclusive_group(required=True)
mode.add_argument("--output", type=Path)
mode.add_argument("--sync", action="store_true")
mode.add_argument("--check", action="store_true")
```

- [ ] **Step 4: Move and materialize**

```bash
mkdir -p skills/qedgen
git mv SKILL.md skills/qedgen/SKILL.md
```

Change the manifest source to:

```json
"SKILL.md": "skills/qedgen/SKILL.md"
```

Before syncing, use `git mv` for the four auditor development paths listed in
this task. The relocated checker accepts optional positional inputs and defaults
to repository paths:

```bash
script_dir="$(cd "$(dirname "$0")" && pwd)"
repo_root="$(cd "$script_dir/.." && pwd)"
skill_root="${1:-$repo_root/skills/qedgen-auditor}"
legacy_allowlist="${2:-$script_dir/data/auditor-basis-legacy-allowlist.txt}"
corpus_registry="${3:-$script_dir/data/auditor-basis-corpus-registry.txt}"
for required in "$skill_root/SKILL.md" "$legacy_allowlist" "$corpus_registry"; do
  [[ -f "$required" ]] || { echo "missing knowledge-base input: $required" >&2; exit 1; }
done
```

Update `check-auditor-skill.sh` and `test-auditor-preflight.sh` to call these
paths. Update the hook README to state that ordinary skill installation neither
includes nor enables the hook and to use `integrations/qedgen-auditor-hooks/`
from a source checkout. Change `model-selection.md`, `audit-handbook.md`, and
`workflow_walkthrough.md` so they never imply an installed `hooks/` directory or
an automatically fired hook: the adapter is optional, separately obtained, and
the workflow remains complete when the user selects reasoning manually. Add
subprocess tests proving checker defaults pass and a missing explicit registry
fails with `missing knowledge-base input`.

Then run:

```bash
python3 scripts/package-skills.py --sync
python3 scripts/package-skills.py --check
bash scripts/check-auditor-skill.sh
bash scripts/check-auditor-knowledge-bases.sh
bash scripts/test-auditor-preflight.sh
```

Repeat Step 0 with a fresh evaluator. GREEN requires it to say the hook is not
bundled or enabled, point to the separate source integration, and explain that
normal audit behavior does not depend on it.

- [ ] **Step 5: Verify GREEN and commit**

```bash
python3 -m unittest discover -s scripts/tests -p 'test_*.py'
git add scripts/package-skills.py scripts/skill-distribution.json \
  scripts/tests/test_skill_distribution.py scripts/check-auditor-skill.sh \
  scripts/check-auditor-knowledge-bases.sh scripts/test-auditor-preflight.sh \
  scripts/data integrations/qedgen-auditor-hooks skills/qedgen \
  skills/qedgen-auditor SKILL.md
git commit -m "feat(skills): materialize runtime distribution boundary"
```

### Task 2: Test real fresh installs and migration paths

**Files:**
- Modify: `scripts/test-skills-cli.py`
- Modify: `scripts/tests/test_skill_distribution.py`
- Modify: `.github/workflows/skill-distribution.yml`

**Interfaces:**
- Fresh installation uses repository root; the staged package is the inventory oracle and migration fixture.
- 1.7.0 update follows the moved named skill automatically.
- 1.5.9 first reproduces deleted-path behavior, then explicit re-add succeeds.

- [ ] **Step 1: Write failing real-layout tests**

Change the source under test while retaining staged expected hashes:

```python
        source_under_test = ROOT
        listing = skills("add", source_under_test, "--list", cwd=project)
        skills("add", source_under_test, "--skill", "qedgen", "qedgen-auditor",
               "--agent", "codex", "--yes", cwd=project)
```

Assert `qedgen-auditor-bench` is discoverable but not installed when the two
runtime names are selected. Compare installed files exactly with inventory.

Before migration, add:

```python
            installed_skill = migration / ".agents/skills/qedgen"
            stale = installed_skill / "crates/old-fixture"
            stale.parent.mkdir(parents=True)
            stale.write_text("legacy development content")
            legacy_binary = installed_skill / "bin/qedgen"
            legacy_binary.parent.mkdir(parents=True)
            legacy_binary.write_text("legacy local binary")
            legacy_binary.chmod(0o755)
```

For 1.7.0 assert update changes `skillPath`, removes both files, and the wrapper
returns 127 naming `bash` and installed `install.sh`. For 1.5.9 retain the blocked
update assertion, explicitly re-add from the same repository, then assert the
same inventory, cleanup, lock path, and wrapper instruction.

- [ ] **Step 2: Verify RED with both pinned clients**

```bash
python3 scripts/test-skills-cli.py --cli /tmp/skills-1.7.0/node_modules/skills/bin/cli.mjs --migration
python3 scripts/test-skills-cli.py --cli /tmp/skills-1.5.9/node_modules/skills/bin/cli.mjs \
  --migration --expect-migration-blocked
```

Expected: new real-layout/migration assertions fail, not dependency errors.

- [ ] **Step 3: Add shared exact-inventory and wrapper helpers**

```python
        def installed_files(skill):
            return {p.relative_to(skill).as_posix() for p in skill.rglob("*") if p.is_file()}

        def assert_exact_inventory(installed_root, name):
            prefix = f"skills/{name}/"
            expected = {p.removeprefix(prefix) for p in inventory["files"] if p.startswith(prefix)}
            assert installed_files(installed_root / name) == expected

        def assert_reinstall_required(wrapper, cwd):
            result = subprocess.run([str(wrapper), "--version"], cwd=cwd, env=env,
                                    text=True, capture_output=True, timeout=120)
            assert result.returncode == 127, result
            assert "bash" in result.stderr and "install.sh" in result.stderr, result.stderr
```

Use the helpers in fresh and migration flows without weakening the existing real
QEDGen help/spec/Anchor-Lean-Kani smoke.

- [ ] **Step 4: Add the CI drift gate**

Before distribution unit tests add:

```yaml
      - name: Check committed runtime skill inventory
        run: python3 scripts/package-skills.py --check
```

- [ ] **Step 5: Verify GREEN and commit**

```bash
python3 scripts/package-skills.py --check
python3 -m unittest discover -s scripts/tests -p 'test_*.py'
python3 scripts/test-skills-cli.py --cli /tmp/skills-1.7.0/node_modules/skills/bin/cli.mjs --migration
python3 scripts/test-skills-cli.py --cli /tmp/skills-1.5.9/node_modules/skills/bin/cli.mjs \
  --migration --expect-migration-blocked
git add scripts/test-skills-cli.py scripts/tests/test_skill_distribution.py \
  .github/workflows/skill-distribution.yml
git commit -m "test(skills): enforce public install and migration paths"
```

If the pinned CLIs are absent, install them only into temporary prefixes:

```bash
npm install --ignore-scripts --no-audit --no-fund --prefix /tmp/skills-1.7.0 skills@1.7.0
npm install --ignore-scripts --no-audit --no-fund --prefix /tmp/skills-1.5.9 skills@1.5.9
```

### Task 3: Publish entry points and migration guidance

**Files:**
- Modify: `package.json`
- Modify: `README.md`
- Modify: `docs/index.html`
- Modify: `docs/llms.txt`
- Modify: `docs/RELEASING.md`
- Modify: `docs/skill-distribution.md`
- Modify: `references/installation.md`
- Modify: `scripts/check-closed-issue-refs.sh`
- Modify: `scripts/tests/test_skill_distribution.py`
- Regenerate: `skills/qedgen/**`

**Interfaces:**
- `package.json.main` becomes `skills/qedgen/SKILL.md`.
- Migration docs distinguish fresh install, 1.7.0 update, 1.5.9 upgrade/re-add, binary reinstall, and hook relocation.

- [ ] **Step 1: Capture RED skill behavior before editing instructions**

Per `superpowers:writing-skills`, give independent evaluators the current skill
and minimal install docs, without the desired answers:

1. "I installed qedgen before its SKILL.md moved and use skills 1.5.9. How do I update without keeping development fixtures?"
2. "After updating, tools/qedgen says the CLI is missing. Should it use PATH, build the workspace, or reinstall something?"

Record incorrect ordinary-update advice, claims that the executable is bundled,
PATH/source fallback, or omission of explicit reinstall. These observations are
the documentation RED baseline.

- [ ] **Step 2: Write failing metadata and migration tests**

```python
    def test_public_metadata_points_to_runtime_entry(self):
        package = json.loads((ROOT / "package.json").read_text())
        self.assertEqual(package["main"], "skills/qedgen/SKILL.md")
        self.assertIn("skills/qedgen/SKILL.md", package["agentSkills"]["skills"])
        self.assertNotIn("SKILL.md", package["agentSkills"]["skills"])

    def test_migration_guide_names_required_user_actions(self):
        guide = (ROOT / "docs/skill-distribution.md").read_text()
        for text in ("1.7.0", "1.5.9", "re-add", "install.sh", "bin/qedgen",
                     "integrations/qedgen-auditor-hooks"):
            self.assertIn(text, guide)
        self.assertNotIn("This is packaging groundwork", guide)
```

Run them; expect failures on old root metadata and groundwork prose.

- [ ] **Step 3: Update live metadata and links**

Set:

```json
"main": "skills/qedgen/SKILL.md",
"agentSkills": {
  "skills": [
    "skills/qedgen/SKILL.md",
    "skills/qedgen-auditor/SKILL.md",
    "skills/qedgen-auditor-bench/SKILL.md"
  ]
}
```

Change live README/site/raw links from root `SKILL.md` to
`skills/qedgen/SKILL.md`. Update active scripts that scan the root path; do not
rewrite historical notes that merely use the term `SKILL.md` conceptually.

- [ ] **Step 4: Document the exact migration contract**

Replace groundwork text with these facts:

```text
Fresh: npx skills add qedgen/solana-skills
Automatic existing update: Skills CLI >= 1.7.0
Skills CLI 1.5.9: upgrade or explicitly re-add from the same repository
After project-local replacement: run bash .agents/skills/qedgen/install.sh once; bin/qedgen is local runtime state
Optional hook: relocate or disable the old installed-path setting; adapter source is integrations/qedgen-auditor-hooks
```

Preserve the provider-scan limitation and route appeal/rescan work to #412. Do
not imply that the wrapper installs, searches PATH, or builds automatically.

- [ ] **Step 5: Sync and verify GREEN skill behavior**

```bash
python3 scripts/package-skills.py --sync
python3 scripts/package-skills.py --check
```

Repeat both evaluator scenarios with fresh agents. GREEN requires correct 1.7.0
versus 1.5.9 guidance, recognition that the executable is downloaded local
state, and the explicit installer action. Tighten only prose tied to observed
failures and re-sync.

- [ ] **Step 6: Run gates and commit**

```bash
python3 -m unittest discover -s scripts/tests -p 'test_*.py'
bash scripts/check-version-consistency.sh
bash scripts/check-auditor-skill.sh
bash scripts/check-closed-issue-refs.sh
git diff --check
git add package.json README.md docs references/installation.md \
  scripts/check-closed-issue-refs.sh scripts/tests/test_skill_distribution.py skills/qedgen
git commit -m "docs(skills): publish runtime layout migration"
```

If authenticated closed-issue checking is unavailable, report that exact
limitation rather than claiming it passed.

### Task 4: End-to-end verification and handoff

**Files:**
- Verify only; change earlier task files only for reproduced failures.

**Interfaces:**
- Consumes all previous deliverables.
- Produces evidence that development fixtures remain intact and installed packages work without source fixtures.

- [ ] **Step 1: Verify retention and exclusion**

```bash
test -f crates/qedgen/tests/fixtures/regressions/v2.21-crucible-crash-first/buggy_anchor/idl.json
test ! -e skills/qedgen/crates
test ! -e skills/qedgen-auditor/hooks
python3 scripts/package-skills.py --check
```

- [ ] **Step 2: Run the full repository gate**

```bash
npm test
```

Expected: all existing gates and distribution tests pass; known allowlisted
knowledge-base warnings may remain, but no new failures are accepted.

- [ ] **Step 3: Run real installed-package smoke**

```bash
cargo build --locked --release --bin qedgen
python3 scripts/test-skills-cli.py \
  --cli /tmp/skills-1.7.0/node_modules/skills/bin/cli.mjs \
  --qedgen target/release/qedgen --migration
python3 scripts/test-skills-cli.py \
  --cli /tmp/skills-1.5.9/node_modules/skills/bin/cli.mjs \
  --qedgen target/release/qedgen --migration --expect-migration-blocked
```

Expected: discovery, exact installs, cleanup, migration, wrapper help/version,
spec validation, and Anchor/Lean/Kani generation all pass.

- [ ] **Step 4: Run syntax and diff checks**

```bash
python3 -m py_compile scripts/package-skills.py scripts/test-skills-cli.py \
  scripts/tests/test_skill_distribution.py
bash -n scripts/check-auditor-knowledge-bases.sh \
  integrations/qedgen-auditor-hooks/auditor-thinking-budget.sh
uv run --with pyyaml \
  /Users/abishek/.codex/skills/.system/skill-creator/scripts/quick_validate.py \
  skills/qedgen
uv run --with pyyaml \
  /Users/abishek/.codex/skills/.system/skill-creator/scripts/quick_validate.py \
  skills/qedgen-auditor
git diff --check
git status --short
```

- [ ] **Step 5: Request whole-branch review and finish**

Use `superpowers:requesting-code-review` from base `7c8fbf1b` to HEAD, focusing
on accidental source inclusion, rollback, migration accuracy, local binaries,
and hook migration. Fix confirmed important findings test-first and repeat the
affected verification. Then use `superpowers:verification-before-completion`
and `superpowers:finishing-a-development-branch`.

The PR must state that it closes #407, preserves the install command, requires
Skills CLI 1.7.0+ only for automatic migration, documents 1.5.9 re-add and the
one-time binary reinstall, retains fixtures, and does not claim provider
reclassification.
