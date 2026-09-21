#!/usr/bin/env python3
"""Build, synchronize, or check the explicit portable skill inventory."""
import argparse
import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import re
import shutil
import subprocess
import tempfile


ROOT = Path(__file__).resolve().parent.parent
PUBLIC_SKILL_NAMES = ("qedgen", "qedgen-auditor")
RUNTIME_BIN = PurePosixPath("bin/qedgen")
RUNTIME_TEMP_RE = re.compile(r"^\.(?:qedgen|checksum)\.[A-Za-z0-9]+$")


def relative_path(value):
    path = PurePosixPath(value)
    if not value or path.is_absolute() or ".." in path.parts or "\\" in value:
        raise ValueError(f"unsafe manifest path: {value!r}")
    return path


def is_runtime_state(relative):
    relative = PurePosixPath(relative)
    if relative == RUNTIME_BIN:
        return True
    return (
        len(relative.parts) == 2
        and relative.parts[0] == "bin"
        and bool(RUNTIME_TEMP_RE.fullmatch(relative.parts[1]))
    )


def validate_links(skill):
    skill = skill.resolve()
    for source in skill.rglob("*.md"):
        for target in re.findall(r"\]\(([^\s)]+)\)", source.read_text()):
            if ":" in target or target.startswith("#"):
                continue
            resolved = (source.parent / target.split("#", 1)[0]).resolve()
            if not resolved.is_relative_to(skill) or not resolved.exists():
                raise ValueError(
                    f"broken package reference: {source.relative_to(skill)} -> {target}"
                )


def file_snapshot(root, ignore_runtime_bin=False):
    result = {}
    for path in sorted(root.rglob("*")):
        if not path.is_file():
            continue
        relative = path.relative_to(root)
        if ignore_runtime_bin and is_runtime_state(relative):
            continue
        result[relative.as_posix()] = (
            hashlib.sha256(path.read_bytes()).hexdigest(),
            path.stat().st_mode & 0o111,
        )
    return result


def stage_package(stage, root=ROOT):
    manifest_path = root / "scripts/skill-distribution.json"
    manifest = json.loads(manifest_path.read_text())
    if manifest["schema_version"] != 1:
        raise ValueError("unsupported distribution manifest version")
    version = json.loads((root / "package.json").read_text())["version"]
    cargo = (root / "crates/qedgen/Cargo.toml").read_text()
    match = re.search(r'^version\s*=\s*"([^"]+)"', cargo, re.MULTILINE)
    auditor_version = (root / "skills/qedgen-auditor/VERSION").read_text().strip()
    if not match or version != match[1] or version != auditor_version:
        raise ValueError("version metadata drift; run scripts/check-version-consistency.sh")
    if not re.fullmatch(r"\d+\.\d+\.\d+(?:[-+][0-9A-Za-z.-]+)?", version):
        raise ValueError("invalid release version")

    for name, files in manifest["skills"].items():
        if name not in PUBLIC_SKILL_NAMES:
            raise ValueError(f"unexpected portable skill: {name}")
        skill = stage / "skills" / name
        for destination, source in files.items():
            target = skill / relative_path(destination)
            origin = root / relative_path(source)
            if (
                not origin.resolve().is_relative_to(root.resolve())
                or not origin.is_file()
                or origin.is_symlink()
            ):
                raise ValueError(f"manifest source must be a repository file: {source}")
            excluded = {
                "crates",
                "fixtures",
                "hooks",
                "examples",
                "target",
                ".git",
                "qedgen-auditor-bench",
            }
            if excluded.intersection(origin.relative_to(root).parts) or excluded.intersection(
                target.relative_to(stage).parts
            ):
                raise ValueError(f"development-only path in distribution: {source} -> {destination}")
            target.parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(origin, target)
            target.chmod(origin.stat().st_mode & 0o777)
        (skill / "VERSION").write_text(version + "\n")
        if not re.search(
            rf"^name: {re.escape(name)}$", (skill / "SKILL.md").read_text(), re.MULTILINE
        ):
            raise ValueError(f"skill name mismatch: {name}")
        validate_links(skill)

    files = {
        path.relative_to(stage).as_posix(): hashlib.sha256(path.read_bytes()).hexdigest()
        for path in sorted(stage.rglob("*"))
        if path.is_file()
    }
    commit = subprocess.run(
        ["git", "-C", str(root), "rev-parse", "HEAD"],
        text=True,
        capture_output=True,
        check=True,
    ).stdout.strip()
    inventory = {
        "schema_version": 1,
        "version": version,
        "source_commit": commit,
        "manifest_sha256": hashlib.sha256(manifest_path.read_bytes()).hexdigest(),
        "skills": sorted(manifest["skills"]),
        "files": files,
    }
    (stage / "distribution.json").write_text(
        json.dumps(inventory, indent=2, sort_keys=True) + "\n"
    )
    return inventory


def build(output, root=ROOT):
    output = output.absolute()
    if output.exists() or output.is_symlink():
        raise ValueError(f"output already exists: {output}")
    if output.resolve().is_relative_to(root.resolve()):
        raise ValueError("stage outside the repository so public skills cannot copy the output")
    output.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix=".qedgen-distribution-", dir=output.parent) as scratch:
        stage = Path(scratch) / "package"
        stage.mkdir()
        inventory = stage_package(stage, root)
        stage.rename(output)
    print(f"Staged {len(inventory['files'])} files for {', '.join(inventory['skills'])} in {output}")


def check_public_skills(root=ROOT):
    with tempfile.TemporaryDirectory(prefix="qedgen-public-check-") as scratch:
        stage = Path(scratch) / "package"
        stage.mkdir()
        stage_package(stage, root)
        for name in PUBLIC_SKILL_NAMES:
            expected = file_snapshot(stage / "skills" / name)
            actual = file_snapshot(root / "skills" / name, name == "qedgen")
            if actual != expected:
                missing = sorted(expected.keys() - actual.keys())
                extra = sorted(actual.keys() - expected.keys())
                changed = sorted(
                    path
                    for path in expected.keys() & actual.keys()
                    if expected[path] != actual[path]
                )
                raise ValueError(
                    f"committed public skill drift: {name}; run --sync "
                    f"(missing={missing}, extra={extra}, changed={changed})"
                )
    print("Committed public skill inventory is current.")


def sync_public_skills(root=ROOT):
    public_root = root / "skills"
    public_root.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="qedgen-public-stage-", dir=root.parent) as scratch:
        stage = Path(scratch) / "package"
        stage.mkdir()
        stage_package(stage, root)
        backup_root = Path(tempfile.mkdtemp(prefix=".qedgen-public-backup-", dir=public_root))
        backed_up = []
        rollback_failed = False
        try:
            for name in PUBLIC_SKILL_NAMES:
                target = public_root / name
                backup = backup_root / name
                target.rename(backup)
                backed_up.append(name)
                if (
                    os.environ.get("QEDGEN_PACKAGE_TESTING") == "1"
                    and os.environ.get("QEDGEN_TEST_SYNC_FAIL_BEFORE_INSTALL") == name
                ):
                    raise ValueError(f"injected sync failure before installing {name}")
                (stage / "skills" / name).rename(target)
                if (
                    os.environ.get("QEDGEN_PACKAGE_TESTING") == "1"
                    and os.environ.get("QEDGEN_TEST_SYNC_FAIL_AFTER") == name
                ):
                    raise ValueError(f"injected sync failure after {name}")

            legacy_bin = backup_root / "qedgen/bin"
            if legacy_bin.exists():
                destination = public_root / "qedgen/bin"
                destination.mkdir(parents=True, exist_ok=True)
                for path in legacy_bin.iterdir():
                    relative = PurePosixPath("bin") / path.name
                    if is_runtime_state(relative):
                        path.rename(destination / path.name)
        except Exception as sync_error:
            rollback_errors = []
            for name in reversed(backed_up):
                try:
                    target = public_root / name
                    if target.exists():
                        shutil.rmtree(target)
                    backup = backup_root / name
                    if backup.exists():
                        backup.rename(target)
                except Exception as rollback_error:
                    rollback_errors.append(f"{name}: {rollback_error}")
            if rollback_errors:
                rollback_failed = True
                raise RuntimeError(
                    f"sync failed ({sync_error}); rollback incomplete; backups retained "
                    f"at {backup_root}: {'; '.join(rollback_errors)}"
                ) from sync_error
            raise
        finally:
            if not rollback_failed:
                shutil.rmtree(backup_root, ignore_errors=True)
    print("Synchronized committed public skill inventory.")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    mode = parser.add_mutually_exclusive_group(required=True)
    mode.add_argument("--output", type=Path, help="new directory outside the repository")
    mode.add_argument("--sync", action="store_true", help="synchronize committed public skills")
    mode.add_argument("--check", action="store_true", help="check committed public skills for drift")
    parser.add_argument("--root", type=Path, help=argparse.SUPPRESS)
    args = parser.parse_args()
    try:
        root = ROOT
        if args.root is not None:
            if os.environ.get("QEDGEN_PACKAGE_TESTING") != "1":
                raise ValueError("--root is available only to package tests")
            root = args.root.resolve()
        if args.output is not None:
            build(args.output, root)
        elif args.sync:
            sync_public_skills(root)
        else:
            check_public_skills(root)
    except (ValueError, OSError, KeyError, subprocess.CalledProcessError) as error:
        parser.exit(1, f"error: {error}\n")


if __name__ == "__main__":
    main()
