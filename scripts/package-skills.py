#!/usr/bin/env python3
"""Stage portable skills from an explicit allowlist without changing discovery."""
import argparse
import hashlib
import json
from pathlib import Path, PurePosixPath
import re
import shutil
import subprocess
import tempfile


ROOT = Path(__file__).resolve().parent.parent
MANIFEST = ROOT / "scripts/skill-distribution.json"


def relative_path(value):
    path = PurePosixPath(value)
    if not value or path.is_absolute() or ".." in path.parts or "\\" in value:
        raise ValueError(f"unsafe manifest path: {value!r}")
    return path


def validate_links(skill):
    skill = skill.resolve()
    for source in skill.rglob("*.md"):
        for target in re.findall(r"\]\(([^\s)]+)\)", source.read_text()):
            if ":" in target or target.startswith("#"):
                continue
            resolved = (source.parent / target.split("#", 1)[0]).resolve()
            if not resolved.is_relative_to(skill) or not resolved.exists():
                raise ValueError(f"broken package reference: {source.relative_to(skill)} -> {target}")


def build(output):
    output = output.absolute()
    if output.exists() or output.is_symlink():
        raise ValueError(f"output already exists: {output}")
    if output.resolve().is_relative_to(ROOT):
        raise ValueError("stage outside the repository so legacy root installs cannot copy the output")
    manifest = json.loads(MANIFEST.read_text())
    if manifest["schema_version"] != 1:
        raise ValueError("unsupported distribution manifest version")
    version = json.loads((ROOT / "package.json").read_text())["version"]
    cargo = (ROOT / "crates/qedgen/Cargo.toml").read_text()
    match = re.search(r'^version\s*=\s*"([^"]+)"', cargo, re.MULTILINE)
    if not match or version != match[1] or version != (ROOT / "skills/qedgen-auditor/VERSION").read_text().strip():
        raise ValueError("version metadata drift; run scripts/check-version-consistency.sh")
    if not re.fullmatch(r"\d+\.\d+\.\d+(?:[-+][0-9A-Za-z.-]+)?", version):
        raise ValueError("invalid release version")
    output.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix=".qedgen-distribution-", dir=output.parent) as scratch:
        stage = Path(scratch) / "package"
        stage.mkdir()
        for name, files in manifest["skills"].items():
            if name not in {"qedgen", "qedgen-auditor"}:
                raise ValueError(f"unexpected portable skill: {name}")
            skill = stage / "skills" / name
            for destination, source in files.items():
                target = skill / relative_path(destination)
                origin = ROOT / relative_path(source)
                if not origin.resolve().is_relative_to(ROOT) or not origin.is_file() or origin.is_symlink():
                    raise ValueError(f"manifest source must be a repository file: {source}")
                excluded = {"crates", "fixtures", "hooks", "examples", "target", ".git", "qedgen-auditor-bench"}
                if excluded.intersection(origin.relative_to(ROOT).parts) or excluded.intersection(target.relative_to(stage).parts):
                    raise ValueError(f"development-only path in distribution: {source} -> {destination}")
                target.parent.mkdir(parents=True, exist_ok=True)
                shutil.copyfile(origin, target)
                target.chmod(origin.stat().st_mode & 0o777)
            (skill / "VERSION").write_text(version + "\n")
            if not re.search(rf"^name: {re.escape(name)}$", (skill / "SKILL.md").read_text(), re.MULTILINE):
                raise ValueError(f"skill name mismatch: {name}")
            validate_links(skill)
        files = {p.relative_to(stage).as_posix(): hashlib.sha256(p.read_bytes()).hexdigest()
                 for p in sorted(stage.rglob("*")) if p.is_file()}
        commit = subprocess.run(["git", "-C", str(ROOT), "rev-parse", "HEAD"],
                                text=True, capture_output=True, check=True).stdout.strip()
        # Hashes describe staged bytes, including uncommitted source edits;
        # the commit is provenance, not a claim of a clean release tree.
        inventory = {"schema_version": 1, "version": version, "source_commit": commit,
                     "manifest_sha256": hashlib.sha256(MANIFEST.read_bytes()).hexdigest(),
                     "skills": sorted(manifest["skills"]), "files": files}
        (stage / "distribution.json").write_text(json.dumps(inventory, indent=2, sort_keys=True) + "\n")
        stage.rename(output)
    print(f"Staged {len(files)} files for {', '.join(inventory['skills'])} in {output}")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, required=True, help="new directory outside the repository")
    args = parser.parse_args()
    try:
        build(args.output)
    except (ValueError, OSError, KeyError, subprocess.CalledProcessError) as error:
        parser.exit(1, f"error: {error}\n")


if __name__ == "__main__":
    main()
