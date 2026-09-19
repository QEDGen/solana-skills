"""Installation side effects and replacement safety, using local command stubs."""
import hashlib
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]


class InstallationTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory(prefix="qedgen-install-test-")
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name).resolve()
        self.skill = self.root / "skill with spaces"
        (self.skill / "tools").mkdir(parents=True)
        shutil.copy2(ROOT / "install.sh", self.skill / "install.sh")
        shutil.copy2(ROOT / "tools/qedgen", self.skill / "tools/qedgen")
        (self.skill / "VERSION").write_text("2.49.0\n")
        self.home = self.root / "home"
        (self.home / ".cargo/bin").mkdir(parents=True)
        self.commands = self.root / "commands"
        self.commands.mkdir()
        self.log = self.root / "commands.log"
        self.payload = self.root / "payload"
        self.checksum = self.root / "checksum"
        self.write_payload('#!/bin/sh\necho "qedgen 2.49.0"\n')
        self.stub("curl", '''echo curl >> "$TEST_LOG"
dest=""
while [ "$#" -gt 0 ]; do
  case "$1" in -o) dest="$2"; shift 2 ;; *) url="$1"; shift ;; esac
done
case "$url" in
  *.sha256) [ "$TEST_FAIL" != checksum ] || exit 22; cp "$TEST_CHECKSUM" "$dest" ;;
  *) [ "$TEST_FAIL" != binary ] || exit 22; cp "$TEST_PAYLOAD" "$dest" ;;
esac
''')
        self.stub("cargo", '''echo cargo >> "$TEST_LOG"
mkdir -p "$TEST_SKILL/target/release"
cp "$TEST_PAYLOAD" "$TEST_SKILL/target/release/qedgen"
''')
        self.stub("uname", '''case "$1" in -s) echo "${TEST_OS:-Linux}" ;; -m) echo x86_64 ;; esac
''')
        self.env = dict(os.environ, HOME=str(self.home), PATH=str(self.commands) + ":/usr/bin:/bin",
                        TEST_LOG=str(self.log), TEST_PAYLOAD=str(self.payload),
                        TEST_CHECKSUM=str(self.checksum), TEST_SKILL=str(self.skill), TEST_FAIL="")

    def stub(self, name, body):
        path = self.commands / name
        path.write_text("#!/bin/sh\nset -e\n" + body)
        path.chmod(0o755)

    def write_payload(self, body):
        self.payload.write_text(body)
        self.checksum.write_text(hashlib.sha256(self.payload.read_bytes()).hexdigest() + "  qedgen\n")

    def existing_binary(self):
        binary = self.skill / "bin/qedgen"
        binary.parent.mkdir(exist_ok=True)
        binary.write_text('#!/bin/sh\necho "qedgen 2.48.0"\n')
        binary.chmod(0o755)
        return binary, binary.read_bytes()

    def source_checkout(self):
        manifest = self.skill / "crates/qedgen/Cargo.toml"
        manifest.parent.mkdir(parents=True)
        manifest.write_text('[package]\nversion = "2.49.0"\n')
        (self.skill / "Cargo.toml").write_text('[workspace]\n')

    def run_install(self, *args):
        return subprocess.run(["/bin/bash", str(self.skill / "install.sh"), *args],
                              env=self.env, capture_output=True, text=True, timeout=10)

    def assert_no_commands(self):
        self.assertFalse(self.log.exists(), self.log.read_text() if self.log.exists() else "")

    def test_missing_wrapper_does_not_install(self):
        result = subprocess.run([str(self.skill / "tools/qedgen"), "--help"], env=self.env,
                                capture_output=True, text=True)
        self.assertEqual(result.returncode, 127, result.stdout + result.stderr)
        self.assertIn("install.sh", result.stderr)
        self.assert_no_commands()
        self.assertFalse((self.skill / "bin").exists())

    def test_help_is_read_only(self):
        result = self.run_install("--help")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("--link-dir", result.stdout)
        self.assert_no_commands()

    def test_default_install_does_not_write_home_links(self):
        result = self.run_install()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertTrue((self.skill / "bin/qedgen").is_file())
        self.assertFalse((self.home / ".local").exists())
        self.assertFalse((self.home / ".cargo/bin/qedgen").exists())

    def test_installed_binary_is_executable_for_every_user(self):
        # The candidate comes from mktemp, which creates 0600. `chmod +x`
        # alone would leave a 0700 CLI: unusable through a --link-dir in a
        # shared directory, or by any other account reading the skill.
        for args in ([], ["--from-source"]):
            with self.subTest(args=args):
                self.source_checkout()
                result = self.run_install(*args)
                self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
                mode = (self.skill / "bin/qedgen").stat().st_mode & 0o777
                self.assertEqual(mode, 0o755, oct(mode))
                shutil.rmtree(self.skill / "bin")
                shutil.rmtree(self.skill / "crates")

    def test_path_link_requires_explicit_directory(self):
        destination = self.home / "chosen bin"
        result = self.run_install("--link-dir", str(destination))
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertEqual((destination / "qedgen").resolve(), self.skill / "bin/qedgen")
        self.assertFalse((self.home / ".cargo/bin/qedgen").exists())

    def test_link_conflict_is_not_overwritten(self):
        destination = self.home / "chosen"
        destination.mkdir()
        existing = destination / "qedgen"
        existing.write_text("user-managed executable")
        result = self.run_install("--link-dir", str(destination))
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(existing.read_text(), "user-managed executable")
        self.assert_no_commands()

    def test_checksum_failure_preserves_binary_and_never_falls_back(self):
        self.source_checkout()
        binary, before = self.existing_binary()
        self.checksum.write_text("0" * 64 + "  qedgen\n")
        result = self.run_install()
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(binary.read_bytes(), before)
        self.assertNotIn("cargo", self.log.read_text())

    def test_missing_checksum_preserves_binary_and_never_falls_back(self):
        self.source_checkout()
        binary, before = self.existing_binary()
        self.env["TEST_FAIL"] = "checksum"
        result = self.run_install()
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(binary.read_bytes(), before)
        self.assertNotIn("cargo", self.log.read_text())

    def test_wrong_version_and_nonrunning_candidates_preserve_binary(self):
        binary, before = self.existing_binary()
        for body in ('#!/bin/sh\necho "qedgen 0.0.1"\n', '#!/bin/sh\nexit 7\n'):
            with self.subTest(body=body):
                self.write_payload(body)
                result = self.run_install()
                self.assertNotEqual(result.returncode, 0)
                self.assertEqual(binary.read_bytes(), before)

    def test_network_failure_preserves_portable_binary(self):
        binary, before = self.existing_binary()
        self.env["TEST_FAIL"] = "binary"
        result = self.run_install()
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(binary.read_bytes(), before)

    def test_unsupported_platform_requires_explicit_source_build(self):
        self.source_checkout()
        self.env["TEST_OS"] = "UnsupportedOS"
        result = self.run_install()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("--from-source", result.stdout + result.stderr)
        self.assert_no_commands()

    def test_explicit_source_build_skips_downloads(self):
        self.source_checkout()
        result = self.run_install("--from-source")
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertEqual(self.log.read_text().strip(), "cargo")


if __name__ == "__main__":
    unittest.main()
