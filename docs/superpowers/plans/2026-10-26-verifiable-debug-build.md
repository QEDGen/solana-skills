# Verifiable Debug Build Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add `qedgen build`, which runs one `solana-verify build` and keeps the stripped deploy `.so`, the unstripped debug `.so` and a build record tied to a git commit. Also add a deploy check that flags an on-chain program whose hash differs from the record.

**Architecture:** There is a new directory module `crates/qedgen/src/build/`, with preflight checks, the `solana-verify` command, a self-check plus hash, and the record and orchestrator. The ELF section reading extends the existing hand-written reader in `verify/sbpf_elf.rs`, so no new dependency is needed. `verify` copies a fresh build record into `VerifyEvidence` (schema 4), and a new `verify --check-deployed <PROGRAM_ID>` reuses the `solana program dump` fetcher in `verify/upstream_check.rs`.

**Tech Stack:** Rust (clap, serde, serde_json, sha2, toml, tempfile; all are already dependencies), external `solana-verify` ≥ 0.5.2, Docker.

**Spec:** `docs/superpowers/specs/2026-10-26-verifiable-debug-build-design.md`

## Global Constraints

- Defaults: platform-tools `v1.57`, arch `v3`.
- Minimum `solana-verify` version: `0.5.2`. qedgen does not install it.
- Program hash = lowercase hex SHA-256 of the deploy file bytes with trailing `0x00` bytes stripped (the same as `solana-verify`'s `get_binary_hash`). No `sha256:` prefix, so it matches what `solana-verify` prints.
- On a non-amd64 host, run `solana-verify` with env `DOCKER_DEFAULT_PLATFORM=linux/amd64`.
- Unstripped binary path: `target/sbpf-solana-solana/release/<lib>.so` for arch `v0`, and `target/sbpf<arch>-solana-solana/release/<lib>.so` otherwise (`v3` gives `sbpfv3-solana-solana`).
- Output dir: `<workspace>/.qed/build/`, files `<lib>.so`, `<lib>.debug.so`, `build.json`.
- No local-rebuild fallback, and no fallback when Docker or `solana-verify` is missing: refuse with a fix-it message.
- A deploy mismatch is a P2 finding (warning, exit 0). It never edits the stamp, the evidence or the build record.
- A missing build record never blocks `stamp`.
- Prose (docs, comments, messages) in plain simple sentences, with no em dashes.
- Commits: one line, conventional prefix, no trailers. Commit only when the user has asked. Otherwise stage and report.
- Every new emitter of generated Rust goes through `write_generated_file` (none expected here).

## Review Focus

1. **Program in a sub-crate of a workspace** (`programs/<name>/` under a root `Cargo.toml` with `[workspace]`). `target/` and `[profile.release]` live at the workspace root, not the crate. Expected: both are found at the root. Test in Task 3 (`workspace_root_found_from_member_crate`) and Task 5.
2. **`debug` set only in `[profile.release.build-override]`** (the Anchor template has that table). Expected: refused, because build scripts do not count. Test in Task 3 (`build_override_debug_does_not_count`).
3. **A file with no trailing zeros, or one made only of zeros.** Expected: the hash is unchanged for the first. The second hashes the empty input and does not panic. Test in Task 2.
4. **The build record's commit is not `HEAD`** (a build from an older commit, then new commits). Expected: `verify` warns that the build is stale and does not copy it into the evidence. Test in Task 6 (`stale_build_record_not_copied`).
5. **`solana-verify` prints a version with a suffix or a newer minor** (`solana-verify 0.5.10`). Expected: accepted. A plain string compare would wrongly reject `0.5.10 < 0.5.2`. Test in Task 3 (`version_compare_is_numeric`).

---

### Task 1: ELF section reader

**Files:**
- Modify: `crates/qedgen/src/verify/sbpf_elf.rs`
- Create: `crates/qedgen/tests/fixtures/sbpf-elf/counter-v3-debug.so`

**Interfaces:**
- Produces: `pub fn section_bytes<'a>(elf: &'a [u8], name: &str) -> Result<Option<&'a [u8]>>` (returns `None` when the section is absent, and errors on a malformed ELF), and `pub fn sbpf_version_of(elf: &[u8]) -> Result<u32>` (the same check as `read_sbpf_version`, on bytes).

- [ ] **Step 1: Create the debug fixture**

From `counter-v3.so`, add dummy debug sections without touching `.text`:

```bash
cd crates/qedgen/tests/fixtures/sbpf-elf
printf 'dbg' > /tmp/dbg.bin
~/.cache/solana/v1.57/platform-tools/llvm/bin/llvm-objcopy \
  --add-section .debug_info=/tmp/dbg.bin --add-section .debug_line=/tmp/dbg.bin \
  counter-v3.so counter-v3-debug.so
```

Expected: `llvm-readelf -S counter-v3-debug.so` lists `.debug_info` and `.debug_line`. Add a line to the fixture README (or a comment in the test) that says how it was made.

- [ ] **Step 2: Write the failing tests** (in the existing `#[cfg(test)] mod tests`)

```rust
#[test]
fn text_section_matches_between_stripped_and_debug_fixture() {
    let a = std::fs::read(fixture("counter-v3.so")).unwrap();
    let b = std::fs::read(fixture("counter-v3-debug.so")).unwrap();
    let ta = section_bytes(&a, ".text").unwrap().expect("has .text");
    let tb = section_bytes(&b, ".text").unwrap().expect("has .text");
    assert!(!ta.is_empty());
    assert_eq!(ta, tb);
}

#[test]
fn debug_sections_present_only_in_debug_fixture() {
    let a = std::fs::read(fixture("counter-v3.so")).unwrap();
    let b = std::fs::read(fixture("counter-v3-debug.so")).unwrap();
    assert!(section_bytes(&a, ".debug_info").unwrap().is_none());
    assert!(section_bytes(&b, ".debug_info").unwrap().is_some());
    assert!(section_bytes(&b, ".debug_line").unwrap().is_some());
}

#[test]
fn v0_and_v3_text_differ() {
    let a = std::fs::read(fixture("counter-v0.so")).unwrap();
    let b = std::fs::read(fixture("counter-v3.so")).unwrap();
    assert_ne!(section_bytes(&a, ".text").unwrap(), section_bytes(&b, ".text").unwrap());
}

#[test]
fn section_bytes_rejects_truncated_elf() {
    let a = std::fs::read(fixture("counter-v3.so")).unwrap();
    assert!(section_bytes(&a[..80], ".text").is_err());
}
```

- [ ] **Step 3: Run the tests to verify they fail**

Run: `cargo test -p qedgen-solana-skills --bin qedgen sbpf_elf`
Expected: FAIL to compile (`section_bytes` not found).

- [ ] **Step 4: Implement `section_bytes` and `sbpf_version_of`**

Parse ELF64 little-endian: `e_shoff` (u64 at 0x28), `e_shentsize` (u16 at 0x3A), `e_shnum` (u16 at 0x3C), `e_shstrndx` (u16 at 0x3E). Each section header gives `sh_name` (u32 at +0), `sh_offset` (u64 at +0x18), `sh_size` (u64 at +0x20). Resolve names through the `.shstrtab` section. Bounds-check every read and return an error on overflow. Reuse `u16_at`/`u64_at` and `check_header`. Make `read_sbpf_version` call `sbpf_version_of`.

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test -p qedgen-solana-skills --bin qedgen sbpf_elf`
Expected: PASS (old and new tests).

- [ ] **Step 6: Stage**

```bash
git add crates/qedgen/src/verify/sbpf_elf.rs crates/qedgen/tests/fixtures/sbpf-elf/counter-v3-debug.so
# commit (if approved): feat(verify): read ELF sections for the build self-check
```

---

### Task 2: Program hash and self-check

**Files:**
- Create: `crates/qedgen/src/build/selfcheck.rs`
- Create: `crates/qedgen/src/build/mod.rs` (module declarations only in this task)
- Modify: `crates/qedgen/src/main.rs` (add `mod build;`)

**Interfaces:**
- Consumes: `sbpf_elf::section_bytes`, `sbpf_elf::sbpf_version_of` (Task 1).
- Produces:
  - `pub fn program_hash(bytes: &[u8]) -> String`: 64 lowercase hex characters, trailing zeros stripped first.
  - `pub fn file_sha256(bytes: &[u8]) -> String`: 64 lowercase hex characters, plain.
  - `pub fn self_check(deploy: &[u8], debug: &[u8], expected_sbpf: u32) -> Result<()>`.
  - `pub fn arch_to_sbpf_version(arch: &str) -> Result<u32>`: `"v0"`→0, `"v1"`→1, `"v2"`→2, `"v3"`→3, anything else is an error.

- [ ] **Step 1: Write the failing tests** (in `selfcheck.rs`)

```rust
#[test]
fn program_hash_strips_trailing_zeros() {
    assert_eq!(program_hash(b"abc\0\0\0"), program_hash(b"abc"));
    assert_eq!(program_hash(b"abc"),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
    assert_ne!(program_hash(b"abc\0"), file_sha256(b"abc\0"));
}

#[test]
fn program_hash_of_all_zeros_is_empty_hash() {
    assert_eq!(program_hash(&[0u8; 16]),
        "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855");
}

#[test]
fn self_check_passes_on_matching_pair() {
    let d = std::fs::read(fixture("counter-v3.so")).unwrap();
    let g = std::fs::read(fixture("counter-v3-debug.so")).unwrap();
    self_check(&d, &g, 3).unwrap();
}

#[test]
fn self_check_rejects_text_mismatch() {
    let d = std::fs::read(fixture("counter-v0.so")).unwrap();
    let g = std::fs::read(fixture("counter-v3-debug.so")).unwrap();
    let e = self_check(&d, &g, 0).unwrap_err().to_string();
    assert!(e.contains(".text"), "{e}");
}

#[test]
fn self_check_rejects_missing_debug_info() {
    let d = std::fs::read(fixture("counter-v3.so")).unwrap();
    let e = self_check(&d, &d, 3).unwrap_err().to_string();
    assert!(e.contains("debug"), "{e}");
}

#[test]
fn self_check_rejects_wrong_sbpf_version() {
    let d = std::fs::read(fixture("counter-v3.so")).unwrap();
    let g = std::fs::read(fixture("counter-v3-debug.so")).unwrap();
    assert!(self_check(&d, &g, 0).is_err());
}
```

`fixture(name)` resolves `env!("CARGO_MANIFEST_DIR")/tests/fixtures/sbpf-elf/<name>`, the same as in `sbpf_elf.rs`.

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p qedgen-solana-skills --bin qedgen build::selfcheck`
Expected: FAIL to compile.

- [ ] **Step 3: Implement the four functions**

`self_check` errors in this order. The messages are fixed:
- `.text` differs: `"the unstripped binary's .text differs from the deploy .so; the labels would not describe the deployed bytes"`.
- `.debug_info` or `.debug_line` is missing: `"the unstripped binary has no debug info (.debug_info/.debug_line); set debug in [profile.release]"`.
- The sBPF version is not `expected_sbpf`: `"built sBPF v{got}, expected v{expected}"`.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p qedgen-solana-skills --bin qedgen build::selfcheck`
Expected: PASS.

- [ ] **Step 5: Stage**

```bash
git add crates/qedgen/src/build crates/qedgen/src/main.rs
# commit (if approved): feat(build): add program hash and debug-build self-check
```

---

### Task 3: Preflight checks

**Files:**
- Create: `crates/qedgen/src/build/preflight.rs`

**Interfaces:**
- Produces:
  - `pub fn workspace_root(program: &Path) -> Result<PathBuf>`: the nearest ancestor (including `program`) whose `Cargo.toml` has a `[workspace]` table. If there is none, it is `program` itself.
  - `pub fn release_debug_setting(workspace_cargo_toml: &str) -> Option<String>`: the value of `profile.release.debug` as a string (`"1"`, `"true"`, `"line-tables-only"`, …). `None` when it is absent, or set to `0`/`false`/`"none"`. Only `[profile.release]` counts.
  - `pub fn git_head(dir: &Path) -> Result<String>`: the full 40-character SHA from `git rev-parse HEAD`.
  - `pub fn assert_clean_tree(dir: &Path) -> Result<()>`: errors when `git status --porcelain --untracked-files=all` is non-empty, ignoring paths under `.qed/build/`.
  - `pub fn parse_solana_verify_version(stdout: &str) -> Result<(u64, u64, u64)>`: parses `"solana-verify 0.5.2"`.
  - `pub fn version_at_least(v: (u64,u64,u64), min: (u64,u64,u64)) -> bool`.
  - `pub fn check_tools() -> Result<String>`: runs `solana-verify --version` (refuses below 0.5.2 or when missing, with the install hint `cargo install solana-verify --locked`) and `docker info` (refuses when it fails: `"Docker is not running; qedgen build needs Docker for a verifiable build"`). Returns the version string.

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn reads_release_debug() {
    assert_eq!(release_debug_setting("[profile.release]\ndebug = 1\n").as_deref(), Some("1"));
    assert_eq!(release_debug_setting("[profile.release]\ndebug = \"line-tables-only\"\n").as_deref(), Some("line-tables-only"));
    assert_eq!(release_debug_setting("[profile.release]\ndebug = 0\n"), None);
    assert_eq!(release_debug_setting("[profile.release]\nlto = \"fat\"\n"), None);
}

#[test]
fn build_override_debug_does_not_count() {
    let t = "[profile.release]\nlto = \"fat\"\n[profile.release.build-override]\ndebug = 1\n";
    assert_eq!(release_debug_setting(t), None);
}

#[test]
fn workspace_root_found_from_member_crate() {
    let d = tempfile::tempdir().unwrap();
    std::fs::write(d.path().join("Cargo.toml"), "[workspace]\nmembers=[\"programs/*\"]\n").unwrap();
    let p = d.path().join("programs/x");
    std::fs::create_dir_all(&p).unwrap();
    std::fs::write(p.join("Cargo.toml"), "[package]\nname=\"x\"\n").unwrap();
    assert_eq!(workspace_root(&p).unwrap().canonicalize().unwrap(), d.path().canonicalize().unwrap());
}

#[test]
fn version_compare_is_numeric() {
    assert_eq!(parse_solana_verify_version("solana-verify 0.5.10\n").unwrap(), (0,5,10));
    assert!(version_at_least((0,5,10), (0,5,2)));
    assert!(!version_at_least((0,4,9), (0,5,2)));
}

#[test]
fn dirty_tree_is_refused_and_build_dir_is_ignored() {
    // tempdir: git init, commit one file, then:
    // (a) write .qed/build/x.so -> assert_clean_tree is Ok
    // (b) write untracked new.rs -> assert_clean_tree is Err and the message contains "commit"
}
```

Write the last test fully. Set `user.email`/`user.name` per command with `git -c`, so the test does not depend on global config.

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p qedgen-solana-skills --bin qedgen build::preflight`
Expected: FAIL to compile.

- [ ] **Step 3: Implement the functions**

Parse TOML with `toml::Value`. `check_tools` is not unit-tested (it shells out). Keep it thin over the two parsers.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p qedgen-solana-skills --bin qedgen build::preflight`
Expected: PASS.

- [ ] **Step 5: Stage**

```bash
git add crates/qedgen/src/build/preflight.rs crates/qedgen/src/build/mod.rs
# commit (if approved): feat(build): add preflight checks for verifiable builds
```

---

### Task 4: `solana-verify` command

**Files:**
- Create: `crates/qedgen/src/build/solana_verify.rs`

**Interfaces:**
- Produces:
  - `pub struct BuildArgs { pub library: String, pub arch: String, pub tools_version: String }`.
  - `pub fn build_argv(a: &BuildArgs) -> Vec<String>`: exactly `["build", "--library-name", <lib>, "--arch", <arch>, "--cargo-build-sbf-args=--tools-version <v>"]`.
  - `pub fn verify_from_repo_argv(a: &BuildArgs, repo_url: &str, commit: &str, program_id: &str) -> Vec<String>`: `["verify-from-repo", "--program-id", <id>, "--commit-hash", <commit>, "--library-name", <lib>, "--arch", <arch>, "--cargo-build-sbf-args=--tools-version <v>", <repo_url>]`.
  - `pub fn sbpf_target_dir(arch: &str) -> String`: `"sbpf-solana-solana"` for `v0`, `"sbpf{arch}-solana-solana"` otherwise.
  - `pub fn needs_amd64_emulation() -> bool`: `std::env::consts::ARCH != "x86_64"`.
  - `pub fn run(workspace: &Path, a: &BuildArgs) -> Result<()>`: runs `solana-verify` with `build_argv`, in `workspace`, streaming output, and sets `DOCKER_DEFAULT_PLATFORM=linux/amd64` when `needs_amd64_emulation()`. A non-zero exit is an error that names the command.
  - `pub fn library_name(program: &Path) -> Result<String>`: `[lib] name` from the program `Cargo.toml`, else `package.name` with `-` replaced by `_`.

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn build_argv_is_exact() {
    let a = BuildArgs { library: "dbgprog".into(), arch: "v3".into(), tools_version: "v1.57".into() };
    assert_eq!(build_argv(&a), vec!["build","--library-name","dbgprog","--arch","v3",
        "--cargo-build-sbf-args=--tools-version v1.57"]);
}

#[test]
fn target_dir_by_arch() {
    assert_eq!(sbpf_target_dir("v0"), "sbpf-solana-solana");
    assert_eq!(sbpf_target_dir("v3"), "sbpfv3-solana-solana");
}

#[test]
fn library_name_prefers_lib_table() {
    // tempdir Cargo.toml: [package] name="pinocchio-token-program" [lib] name="pinocchio_token_program" -> the lib name
    // tempdir Cargo.toml: [package] name="my-prog" with no [lib] -> "my_prog"
}

#[test]
fn verify_from_repo_argv_carries_build_settings() {
    let a = BuildArgs { library: "p".into(), arch: "v3".into(), tools_version: "v1.57".into() };
    let v = verify_from_repo_argv(&a, "https://github.com/o/r", "abc123", "Prog111");
    assert!(v.windows(2).any(|w| w == ["--commit-hash", "abc123"]));
    assert!(v.contains(&"--cargo-build-sbf-args=--tools-version v1.57".to_string()));
    assert_eq!(v.last().unwrap(), "https://github.com/o/r");
}
```

Write `library_name_prefers_lib_table` fully.

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p qedgen-solana-skills --bin qedgen build::solana_verify`
Expected: FAIL to compile.

- [ ] **Step 3: Implement**

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p qedgen-solana-skills --bin qedgen build::solana_verify`
Expected: PASS.

- [ ] **Step 5: Stage**

```bash
git add crates/qedgen/src/build/solana_verify.rs crates/qedgen/src/build/mod.rs
# commit (if approved): feat(build): drive solana-verify build with pinned tools and arch
```

---

### Task 5: Build record and `qedgen build` command

**Files:**
- Modify: `crates/qedgen/src/build/mod.rs`
- Modify: `crates/qedgen/src/cli.rs` (new `Commands::Build`)
- Modify: `crates/qedgen/src/run.rs` (`command_name_of` arm `"build"`, `dispatch` arm)
- Create: `crates/qedgen/tests/build_cli.rs`

**Interfaces:**
- Consumes: Tasks 2 to 4.
- Produces:

```rust
pub const BUILD_DIR: &str = ".qed/build";
pub const RECORD_FILENAME: &str = "build.json";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BuildRecord {
    pub schema_version: u32,          // 1
    pub commit: String,               // 40-char SHA
    pub library: String,
    pub program_hash: String,         // selfcheck::program_hash of the deploy .so
    pub file_sha256: String,
    pub solana_verify_version: String,
    pub image: Option<String>,        // image solana-verify used, best-effort from its output; None if not found
    pub platform_tools: String,       // "v1.57"
    pub arch: String,                 // "v3"
    pub debug_setting: String,        // value from [profile.release]
    pub build_argv: Vec<String>,      // solana_verify::build_argv
    pub text_matches: bool,           // always true when written
    pub recorded_at_unix: u64,
}

pub struct BuildOptions { pub program: PathBuf, pub library: Option<String>,
                          pub arch: String, pub tools_version: String }

pub fn run_build(opts: &BuildOptions) -> Result<BuildRecord>;
pub fn record_path(workspace: &Path) -> PathBuf;     // <workspace>/.qed/build/build.json
pub fn load_record(workspace: &Path) -> Result<Option<BuildRecord>>;
```

CLI: `qedgen build --program <PATH> [--library <NAME>] [--arch <v0|v1|v2|v3>] [--tools-version <vX.Y>]`, with default arch `v3` and default tools version `v1.57`. Doc comment: "Build the program in Docker with `solana-verify`, keep the stripped deploy `.so` and the unstripped `.so` (debug info, the label source), and write a build record tied to the current git commit."

`run_build` order: `require_git_repo` → `workspace_root` → `release_debug_setting` (refuse when `None`: `"set debug = 1 (or \"line-tables-only\") in [profile.release] of <path>/Cargo.toml and commit it; a verifier rebuild must produce the same labels"`) → `assert_clean_tree` → `check_tools` → `git_head` → `solana_verify::run` → read the two files → `self_check(deploy, debug, arch_to_sbpf_version(arch)?)` → copy to `.qed/build/<lib>.so` and `<lib>.debug.so` → write `build.json` (pretty JSON plus a trailing newline) → print the commit, the program hash and the `.qed/build/` path.

`run_build` creates `.qed/build/.gitignore` with the contents `*.so\n`, so the binaries stay out of git while `build.json` can be committed.

- [ ] **Step 1: Write the failing CLI tests** (`tests/build_cli.rs`, same style as `tests/stamp_cli.rs`)

```rust
#[test]
fn refuses_without_release_debug() {
    // temp git repo; Cargo.toml with [package] and no [profile.release]; committed
    // run: qedgen build --program <dir>
    // assert: !status.success(); stderr contains "[profile.release]"
}

#[test]
fn refuses_dirty_tree() {
    // temp git repo; Cargo.toml with [profile.release] debug = 1, committed; then an untracked src/new.rs
    // assert: !status.success(); stderr contains "commit"
}

#[test]
fn refuses_outside_git() {
    // temp dir, no git
    // assert: !status.success(); stderr contains "git"
}
```

Plus a unit test in `build/mod.rs`:

```rust
#[test]
fn record_round_trips() {
    // write a BuildRecord to tempdir record_path; load_record returns Some(equal record)
    // load_record on an empty dir returns None
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p qedgen-solana-skills --test build_cli && cargo test -p qedgen-solana-skills --bin qedgen build::tests`
Expected: FAIL (no `build` subcommand).

- [ ] **Step 3: Implement the CLI, dispatch and `run_build`**

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p qedgen-solana-skills --test build_cli && cargo test -p qedgen-solana-skills --bin qedgen build::`
Expected: PASS. The refusal tests never reach Docker.

- [ ] **Step 5: Stage**

```bash
git add crates/qedgen/src/build crates/qedgen/src/cli.rs crates/qedgen/src/run.rs crates/qedgen/tests/build_cli.rs
# commit (if approved): feat(build): add qedgen build for verifiable debug builds
```

---

### Task 6: Evidence link and stamp report

**Files:**
- Modify: `crates/qedgen/src/verify/evidence.rs`
- Modify: `crates/qedgen/src/run_helpers.rs` (`record_verify_evidence`)
- Modify: `crates/qedgen/src/run.rs` (stamp arm: one report line)

**Interfaces:**
- Consumes: `build::load_record`, `build::preflight::{workspace_root, git_head, assert_clean_tree}`.
- Produces:

```rust
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BinaryBuildRef { pub commit: String, pub program_hash: String }

// new field on VerifyEvidence:
#[serde(default, skip_serializing_if = "Option::is_none")]
pub binary_build: Option<BinaryBuildRef>,

pub fn fresh_build_ref(program: &Path) -> Option<BinaryBuildRef>;
```

`fresh_build_ref` returns `Some` only when a record exists, its `commit` equals `git_head`, and `assert_clean_tree` passes. Otherwise it returns `None`, and prints `"warning: build record at <path> is for commit <c>, not HEAD <h>; run qedgen build again"` when a record exists but is stale. `evidence::build` gains a `binary_build: Option<BinaryBuildRef>` parameter and writes `schema_version: 4`. `load` still accepts records of schema 3.

Stamp report line (stderr, after the existing "Evidence:" line), when `binary_build` is `Some`: `"Verified binary: <program_hash> at <commit>"`. When it is `None`, nothing is printed and nothing is blocked.

- [ ] **Step 1: Write the failing tests** (in `evidence.rs` tests)

```rust
#[test]
fn schema3_record_without_binary_build_still_loads() { /* write the JSON of an existing v3 record; load OK; binary_build None */ }

#[test]
fn binary_build_round_trips() { /* build with Some(ref); record; load; field equal; schema_version 4 */ }

#[test]
fn stale_build_record_not_copied() {
    // temp git repo with commit A; write .qed/build/build.json with commit = A; commit again (B)
    // fresh_build_ref(program) == None
    // then a record with commit = B on a clean tree: fresh_build_ref == Some
}
```

And in `tests/stamp_cli.rs`:

```rust
#[test]
fn stamp_works_without_build_record() { /* the existing happy path is unchanged; stderr has no "Verified binary" */ }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p qedgen-solana-skills --bin qedgen evidence && cargo test -p qedgen-solana-skills --test stamp_cli`
Expected: the new tests FAIL.

- [ ] **Step 3: Implement**

Update every `evidence::build` caller (`run_helpers::record_verify_evidence`), passing `program.and_then(fresh_build_ref)`.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p qedgen-solana-skills --bin qedgen evidence && cargo test -p qedgen-solana-skills --test stamp_cli`
Expected: PASS.

- [ ] **Step 5: Stage**

```bash
git add crates/qedgen/src/verify/evidence.rs crates/qedgen/src/run_helpers.rs crates/qedgen/src/run.rs crates/qedgen/tests/stamp_cli.rs
# commit (if approved): feat(verify): bind the verified binary build into evidence
```

---

### Task 7: Deploy check

**Files:**
- Modify: `crates/qedgen/src/verify/upstream_check.rs` (make the fetcher constructible: `pub(crate) fn solana_cli_fetcher(rpc_url: Option<&str>) -> impl BinaryFetcher + '_`)
- Create: `crates/qedgen/src/build/deploy_check.rs`
- Modify: `crates/qedgen/src/cli.rs` (`verify --check-deployed <PROGRAM_ID>`), `crates/qedgen/src/run.rs` (verify arm)

**Interfaces:**
- Consumes: `BuildRecord`, `load_record`, `selfcheck::program_hash`, `upstream_check::BinaryFetcher`.
- Produces:

```rust
#[derive(Debug, PartialEq)]
pub enum DeployCheck {
    Match { program_hash: String },
    Mismatch { on_chain: String, recorded: String, commit: String },
    NotDeployed { program_id: String },
    NoRecord,
}

pub fn check_deployed(record: Option<&BuildRecord>, program_id: &str,
                      fetcher: &mut dyn BinaryFetcher) -> DeployCheck;
pub fn print_deploy_check(c: &DeployCheck);
```

Rules:
- A fetch error whose message contains `"not found"`, `"AccountNotFound"` or `"does not exist"` maps to `NotDeployed`. Other fetch errors return `Err` from the verify arm, so they are not hidden.
- Match and NotDeployed print one info line.
- Mismatch prints `"P2: deployed program <id> hash <on_chain> differs from the verified build <recorded> at commit <commit>; the stamp still covers that commit"`.
- Every case exits 0. `--check-deployed` honors `--rpc-url` and `--offline` (offline: refuse the fetch with the same wording as `check_lock`'s offline error).
- When `--check-deployed` is the only verb, return after the check, as `--check-upstream` does.

Change `check_deployed`'s signature to return `Result<DeployCheck>` if you need it to carry the non-not-found errors. Keep the enum the same.

- [ ] **Step 1: Write the failing tests** (in `deploy_check.rs`, with a fake fetcher like the one in `upstream_check` tests)

```rust
#[test] fn padded_on_chain_bytes_match()  { /* record.program_hash = program_hash(b"elf"); fetcher returns b"elf\0\0\0\0" -> Match */ }
#[test] fn different_bytes_mismatch()     { /* fetcher returns b"other" -> Mismatch with both hashes and commit */ }
#[test] fn missing_account_is_not_deployed() { /* fetcher errs "AccountNotFound" -> NotDeployed */ }
#[test] fn no_record_reported()           { /* record None -> NoRecord, fetcher never called */ }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p qedgen-solana-skills --bin qedgen deploy_check`
Expected: FAIL to compile.

- [ ] **Step 3: Implement, and wire up the CLI flag**

- [ ] **Step 4: Run the tests to verify they pass, plus an end-to-end check through the existing fake-bytes seam**

Run: `cargo test -p qedgen-solana-skills --bin qedgen deploy_check`
Then add to `tests/upstream_check_e2e.rs` (or a new `tests/deploy_check_e2e.rs`) one case that sets `QEDGEN_UPSTREAM_FAKE_BYTES`, writes a `build.json` whose `program_hash` differs, runs `qedgen verify --spec <s> --check-deployed X`, and asserts exit 0 and stderr containing `"P2: deployed program"`.
Expected: PASS.

- [ ] **Step 5: Stage**

```bash
git add crates/qedgen/src/build/deploy_check.rs crates/qedgen/src/verify/upstream_check.rs crates/qedgen/src/cli.rs crates/qedgen/src/run.rs crates/qedgen/tests
# commit (if approved): feat(verify): flag deployed programs that differ from the verified build
```

---

### Task 8: Docker gate, docs and follow-ups

**Files:**
- Create: `crates/qedgen/tests/verifiable_build_gate.rs`
- Modify: `references/cli/validation.md` (`build`, `verify --check-deployed`), `references/cli.md` (command index row), `docs/framework-support.md` (build row), `SKILL.md` (one line in the verify flow)

**Interfaces:**
- Consumes: the `qedgen build` binary.

- [ ] **Step 1: Write the ignored gate test**

`#[ignore]` test `pinocchio_and_anchor_builds_keep_labels`. For one Pinocchio and one Anchor program under `tests/fixtures/` (copy into a temp git repo, add `debug = 1` to `[profile.release]`, commit), run `qedgen build --program <crate>`, then assert:
- the exit is success;
- `.qed/build/<lib>.so` and `<lib>.debug.so` exist;
- `build.json` has `text_matches: true`, `arch: "v3"`, `platform_tools: "v1.57"`;
- the hash line `solana-verify` printed during the build (capture stdout) equals `program_hash`.

If no suitable small Anchor or Pinocchio fixture exists under `tests/fixtures/`, use `examples/rust/escrow/programs/escrow` (Anchor 0.32.1) for Anchor and stop to ask before adding a new Pinocchio fixture.

- [ ] **Step 2: Run it by hand**

Run: `cargo test -p qedgen-solana-skills --test verifiable_build_gate -- --ignored`
Expected: PASS. This needs Docker with about 15 GB free, and takes several minutes under emulation. Also record whether the Anchor 0.32.1 example built through `solana-verify` without changes. The spec asks for that result to be reported back, so put it in the task report.

- [ ] **Step 3: Write the docs**

- `references/cli/validation.md`: a `qedgen build` section (flags, defaults, refusals, outputs, the record fields) and the `--check-deployed` flag. Add `build` to the validation row of the index in `references/cli.md`.
- `docs/framework-support.md`: a build row. Anchor, Pinocchio and native: `solana-verify`. Anchor ≤ 0.31: `anchor verify` does not reproduce, use `solana-verify verify-from-repo`. Quasar: the result from Step 2 if tested, else "not tested". sBPF assembly: not supported.
- `SKILL.md`: one line saying the agent runs `qedgen build` before `qedgen verify` on a program it verifies, and that `debug = 1` in `[profile.release]` costs no code or compute units on platform-tools v1.57.

- [ ] **Step 4: Run the full suite**

Run: `cargo test -p qedgen-solana-skills && cargo clippy --all-targets -- -D warnings`
Expected: PASS, no warnings.

- [ ] **Step 5: Draft the follow-up issues** (do not file them; list them in the final report for the user)

- A CI job for `verifiable_build_gate` (amd64 runner, image cache).
- `verify --check-upstream` hashes without stripping trailing zeros, so a padded pin can mismatch.

- [ ] **Step 6: Stage**

```bash
git add crates/qedgen/tests/verifiable_build_gate.rs references docs/framework-support.md SKILL.md
# commit (if approved): docs(build): document qedgen build and the deploy check
```
