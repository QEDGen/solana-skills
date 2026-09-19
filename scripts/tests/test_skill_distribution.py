"""Portable distribution contracts; no network or real home-directory writes."""
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[2]
BUILDER = ROOT / "scripts/package-skills.py"


class DistributionTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="qedgen-package-test-")
        self.addCleanup(self.temp.cleanup)
        self.scratch = Path(self.temp.name).resolve()
        self.output = self.scratch / "distribution"

    def build(self, *args):
        result = subprocess.run(
            [sys.executable, str(BUILDER), "--output", str(self.output), *args],
            capture_output=True, text=True,
        )
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        return self.output

    def test_inventory_and_integrity(self):
        output = self.build()
        inventory = json.loads((output / "distribution.json").read_text())
        self.assertEqual(set(inventory["skills"]), {"qedgen", "qedgen-auditor"})
        actual = {p.relative_to(output).as_posix() for p in output.rglob("*") if p.is_file()}
        self.assertEqual(actual, set(inventory["files"]) | {"distribution.json"})
        for relative, digest in inventory["files"].items():
            with self.subTest(path=relative):
                path = Path(relative)
                self.assertFalse(set(path.parts) & {"crates", "fixtures", "hooks", "examples", "target", ".git"})
                self.assertNotIn("qedgen-auditor-bench", path.parts)
                self.assertEqual(hashlib.sha256((output / path).read_bytes()).hexdigest(), digest)
        self.assertEqual((output / "skills/qedgen/VERSION").read_text().strip(),
                         json.loads((ROOT / "package.json").read_text())["version"])
        self.assertTrue(os.access(output / "skills/qedgen/tools/qedgen", os.X_OK))
        self.assertTrue((ROOT / "SKILL.md").is_file(), "legacy discovery path must survive")

    def test_relative_markdown_links_are_self_contained(self):
        output = self.build()
        for skill in (output / "skills").iterdir():
            for source in skill.rglob("*.md"):
                for target in re.findall(r"\]\(([^\s)]+)\)", source.read_text()):
                    if ":" in target or target.startswith("#"):
                        continue
                    target = target.split("#", 1)[0]
                    resolved = (source.parent / target).resolve()
                    self.assertTrue(resolved.is_relative_to(skill), (source, target))
                    self.assertTrue(resolved.exists(), (source, target))

    def test_existing_destination_is_not_overwritten(self):
        self.output.mkdir()
        sentinel = self.output / "user-file"
        sentinel.write_text("keep")
        result = subprocess.run([sys.executable, str(BUILDER), "--output", str(self.output)],
                                capture_output=True, text=True)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("already exists", result.stderr)
        self.assertEqual(sentinel.read_text(), "keep")

    def test_output_cannot_be_staged_inside_legacy_skill(self):
        result = subprocess.run([sys.executable, str(BUILDER), "--output", str(ROOT / "unused-package-output")],
                                capture_output=True, text=True)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("outside the repository", result.stderr)
        self.assertFalse((ROOT / "unused-package-output").exists())

    def test_auditor_preflight_without_source_checkout(self):
        output = self.build()
        program = self.scratch / "program"
        (program / "src").mkdir(parents=True)
        (program / "Cargo.toml").write_text('[package]\nname="probe"\nversion="0.1.0"\n[dependencies]\nanchor-lang="0.31.0"\n')
        (program / "src/lib.rs").write_text("// source-only preflight target\n")
        result = subprocess.run(
            ["bash", str(output / "skills/qedgen-auditor/scripts/preflight.sh"),
             "--root", str(program), "--qedgen", str(self.scratch / "missing-qedgen")],
            capture_output=True, text=True, cwd=self.scratch,
        )
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("runtime=anchor", result.stdout)
        self.assertIn("read-only", result.stdout + result.stderr)

    def test_packaged_auditor_validator_self_tests(self):
        output = self.build()
        result = subprocess.run(
            ["bash", str(output / "skills/qedgen-auditor/scripts/check-domain-artifacts.sh")],
            cwd=self.scratch, capture_output=True, text=True,
        )
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

    def installer_environment(self, corrupt=False, unavailable=False):
        self.build()
        skill = self.output / "skills/qedgen"
        version = (skill / "VERSION").read_text().strip()
        commands = self.scratch / "commands"
        commands.mkdir()
        payload = self.scratch / "payload"
        payload.write_text(f'#!/bin/sh\nprintf "qedgen {version}\\n"\n')
        checksum = self.scratch / "checksum"
        checksum.write_text(("0" * 64 if corrupt else hashlib.sha256(payload.read_bytes()).hexdigest()) + "  qedgen\n")
        curl = commands / "curl"
        curl.write_text('''#!/bin/sh
if [ "$TEST_UNAVAILABLE" = 1 ]; then exit 22; fi
dest=""
while [ "$#" -gt 0 ]; do
  case "$1" in
    -o) dest="$2"; shift 2 ;;
    *) url="$1"; shift ;;
  esac
done
case "$url" in
  *"/v$TEST_VERSION/"*) ;;
  *) echo "unpinned download: $url" >&2; exit 2 ;;
esac
case "$url" in
  *.sha256) cp "$TEST_CHECKSUM" "$dest" ;;
  *) cp "$TEST_PAYLOAD" "$dest" ;;
esac
''')
        cargo = commands / "cargo"
        cargo.write_text('#!/bin/sh\necho "unexpected source build" >&2\nexit 99\n')
        curl.chmod(0o755)
        cargo.chmod(0o755)
        home = self.scratch / "home"
        home.mkdir()
        env = dict(os.environ, HOME=str(home), PATH=str(commands) + os.pathsep + os.environ["PATH"],
                   TEST_VERSION=version, TEST_PAYLOAD=str(payload), TEST_CHECKSUM=str(checksum),
                   TEST_UNAVAILABLE="1" if unavailable else "0")
        return skill, env

    def test_portable_installer_and_wrapper(self):
        skill, env = self.installer_environment()
        result = subprocess.run(["bash", str(skill / "install.sh")], env=env, cwd=self.scratch,
                                capture_output=True, text=True)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("Checksum verified", result.stdout)
        result = subprocess.run([str(skill / "tools/qedgen"), "--help"], env=env,
                                capture_output=True, text=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("qedgen " + env["TEST_VERSION"], result.stdout)

    def test_portable_installer_rejects_bad_checksum_without_source_build(self):
        skill, env = self.installer_environment(corrupt=True)
        result = subprocess.run(["bash", str(skill / "install.sh")], env=env, cwd=self.scratch,
                                capture_output=True, text=True)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("checksum mismatch", result.stdout + result.stderr)
        self.assertNotIn("unexpected source build", result.stdout + result.stderr)
        self.assertFalse((skill / "bin/qedgen").exists())

    def test_portable_installer_explains_unavailable_release(self):
        skill, env = self.installer_environment(unavailable=True)
        result = subprocess.run(["bash", str(skill / "install.sh")], env=env, cwd=self.scratch,
                                capture_output=True, text=True)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("source checkout", result.stdout + result.stderr)
        self.assertNotIn("unexpected source build", result.stdout + result.stderr)

    def test_source_checkout_retains_cargo_fallback_and_version_authority(self):
        skill, env = self.installer_environment(unavailable=True)
        manifest = skill / "crates/qedgen/Cargo.toml"
        manifest.parent.mkdir(parents=True)
        manifest.write_text('[package]\nversion = "' + env["TEST_VERSION"] + '"\n')
        (skill / "Cargo.toml").write_text('[workspace]\nmembers = ["crates/qedgen"]\n')
        (skill / "VERSION").write_text("0.0.1\n")
        cargo = self.scratch / "commands/cargo"
        cargo.write_text('''#!/bin/sh
mkdir -p "$TEST_CHECKOUT/target/release"
cp "$TEST_PAYLOAD" "$TEST_CHECKOUT/target/release/qedgen"
''')
        result = subprocess.run(["bash", str(skill / "install.sh")],
                                env=dict(env, TEST_CHECKOUT=str(skill)), cwd=self.scratch,
                                capture_output=True, text=True)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("built from source", result.stdout)
        self.assertIn("qedgen v" + env["TEST_VERSION"] + " installed", result.stdout)


if __name__ == "__main__":
    unittest.main()
