#!/usr/bin/env python3
"""Exercise a supplied skills CLI against isolated packages and Git fixtures.

No CLI is downloaded by this test. Run with --cli /path/to/skills/bin/cli.mjs
and a compatible --runtime (defaults to node). All installs are project-local.
"""
import argparse
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile

ROOT = Path(__file__).resolve().parent.parent


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--cli", type=Path, required=True)
    parser.add_argument("--runtime", default="node")
    parser.add_argument("--qedgen", type=Path, help="also smoke-test a built QEDGen binary")
    parser.add_argument("--migration", action="store_true", help="test root-to-subdirectory update in a disposable Git fixture")
    parser.add_argument("--expect-migration-blocked", action="store_true",
                        help="with --migration, assert the known older-CLI deleted-path behavior")
    args = parser.parse_args()
    if args.expect_migration_blocked and not args.migration:
        parser.error("--expect-migration-blocked requires --migration")
    cli = args.cli.resolve()
    with tempfile.TemporaryDirectory(prefix="qedgen-cli-smoke-") as temporary:
        scratch = Path(temporary).resolve()
        home = scratch / "home"
        home.mkdir()
        env = dict(os.environ, HOME=str(home), XDG_CONFIG_HOME=str(home / ".config"),
                   DISABLE_TELEMETRY="1", CI="1", GIT_CONFIG_GLOBAL=os.devnull,
                   GIT_CONFIG_NOSYSTEM="1", GIT_TERMINAL_PROMPT="0")
        # Do not pass host-specific agent homes or Git context into the fixture.
        for key in ("CODEX_HOME", "CLAUDE_HOME", "GIT_DIR", "GIT_WORK_TREE", "GIT_INDEX_FILE"):
            env.pop(key, None)

        def run(command, cwd=scratch):
            result = subprocess.run([str(x) for x in command], cwd=cwd, env=env,
                                    text=True, capture_output=True, timeout=120)
            if result.returncode:
                raise RuntimeError(f"{command}\n{result.stdout}\n{result.stderr}")
            return result.stdout + result.stderr

        def skills(*command, cwd):
            return run([args.runtime, cli, *command], cwd)

        version = run([args.runtime, cli, "--version"]).strip()
        package = scratch / "package"
        run([sys.executable, ROOT / "scripts/package-skills.py", "--output", package])
        inventory = json.loads((package / "distribution.json").read_text())
        project = scratch / "project"
        project.mkdir()
        listing = skills("add", package, "--list", cwd=project)
        for name in inventory["skills"]:
            assert name in listing, listing
        assert "qedgen-auditor-bench" not in listing, listing
        skills("add", package, "--skill", "qedgen", "qedgen-auditor",
               "--agent", "codex", "--yes", cwd=project)
        installed = project / ".agents/skills"
        for name in inventory["skills"]:
            prefix = f"skills/{name}/"
            expected = {p.removeprefix(prefix) for p in inventory["files"] if p.startswith(prefix)}
            actual = {p.relative_to(installed / name).as_posix()
                      for p in (installed / name).rglob("*") if p.is_file()}
            assert actual == expected, (name, actual ^ expected)
        # Re-add is also the documented refresh route for older CLI releases.
        stale = installed / "qedgen/crates/old-fixture"
        stale.parent.mkdir()
        stale.write_text("legacy copied development content")
        skills("add", package, "--skill", "qedgen", "--agent", "codex", "--yes", cwd=project)
        assert not stale.exists(), "reinstall left stale development content"
        print(f"PASS skills {version}: discovery, independent installs, exact inventory, replacement cleanup")

        if args.qedgen:
            skill = installed / "qedgen"
            (skill / "bin").mkdir()
            shutil.copy2(args.qedgen.resolve(), skill / "bin/qedgen")
            wrapper = skill / "tools/qedgen"
            run([wrapper, "--help"], project)
            assert f"qedgen {inventory['version']}" in run([wrapper, "--version"], project)
            shutil.copyfile(ROOT / "examples/rust/escrow/escrow.qedspec", project / "program.qedspec")
            run(["git", "init", "-q"], project)
            run([wrapper, "init", "--name", "escrow", "--spec", "program.qedspec"], project)
            run([wrapper, "check", "--spec", "program.qedspec"], project)
            run([wrapper, "codegen", "--spec", "program.qedspec", "--target", "anchor",
                 "--all", "--no-check-compiles"], project)
            assert (project / "programs/Cargo.toml").is_file(), "program scaffold missing"
            assert list(project.rglob("*.lean")), "embedded Lean resources missing"
            assert list(project.rglob("*kani*.rs")), "embedded Kani resources missing"
            print("PASS isolated QEDGen wrapper: help, version, spec check, Anchor/Lean/Kani codegen")

        if args.migration:
            remote = scratch / "remote.git"
            remote.mkdir()
            (remote / "SKILL.md").write_text("---\nname: qedgen\ndescription: migration test\n---\nOld root layout.\n")
            run(["git", "init", "-q", "-b", "main"], remote)
            run(["git", "add", "."], remote)

            def commit(message):
                run(["git", "-c", "user.name=Distribution test", "-c", "user.email=test@example.invalid",
                     "-c", "commit.gpgsign=false", "commit", "-qm", message], remote)

            commit("legacy root skill")
            migration = scratch / "migration"
            migration.mkdir()
            source = remote.as_uri()
            skills("add", source, "--skill", "qedgen", "--agent", "codex", "--yes", cwd=migration)
            lock = json.loads((migration / "skills-lock.json").read_text())
            assert lock["skills"]["qedgen"]["skillPath"] == "SKILL.md", lock
            (remote / "SKILL.md").unlink()
            shutil.copytree(package / "skills", remote / "skills")
            run(["git", "add", "-A"], remote)
            commit("minimal skill subdirectories")
            try:
                update_output = skills("update", "qedgen", "--project", "--yes", cwd=migration)
            except RuntimeError:
                # Upstream suppresses the child add process's stderr. Re-run
                # that operation only to explain a failure, never to pass it.
                print(skills("add", source, "--skill", "qedgen", "--full-depth", "--yes", cwd=migration))
                raise
            lock = json.loads((migration / "skills-lock.json").read_text())
            if args.expect_migration_blocked:
                assert lock["skills"]["qedgen"]["skillPath"] == "SKILL.md", update_output
                assert "deleted upstream" in update_output, update_output
                print(f"PASS skills {version}: reproduced legacy migration blocker; keep published root SKILL.md")
                return
            assert lock["skills"]["qedgen"]["skillPath"] == "skills/qedgen/SKILL.md", update_output
            assert (migration / ".agents/skills/qedgen/VERSION").is_file(), update_output
            print(f"PASS skills {version}: root-to-subdirectory project update (local Git transport)")


if __name__ == "__main__":
    main()
