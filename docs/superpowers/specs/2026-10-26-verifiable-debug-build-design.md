# Verifiable debug build (`qedgen build`)

Issue: [#449](https://github.com/QEDGen/solana-skills/issues/449). Follow-up: #450 (end-to-end label-driven lift). Consumer of the output: QEDGen/qedsvm#81 (concept map from debug info).

## Intent

We only verify binaries that we build from source. The label-driven lift (qedsvm#81, #82) reads debug info (DWARF) to name functions, account fields and error codes in the lifted program. That debug info must describe exactly the bytes that get deployed. If it comes from a different build, the labels can point at the wrong code, and the proof says nothing about the deployed program.

So qedgen must own the build. It produces, from one build:

- the stripped `.so` that the author can deploy, and
- the unstripped `.so` with debug info, which is the label source.

It records the build so that anyone can reproduce the same bytes with standard tools, and so that a deployed program can be checked against what we verified.

## Who runs it, and when

- We (the agent driving QEDGen) run the build. The program author does not.
- The build happens before deploy, as part of verification.
- The stamp covers the commit we verified, and only that commit.
- We do not control what the author deploys. We give them the verified stripped `.so` if they want it. If they deploy something else, we flag the mismatch. The stamp is not changed or revoked.

## Spike findings (2026-10-26)

All measured, see the three spike comments on #449. Summary:

1. **Within one build, the unstripped `.text` is byte-identical to the deployed `.so`.** True for every build tested (V0 and v3, local and Docker, Anchor and Pinocchio).
2. **Debug info and the code.** On platform-tools v1.57, `debug = 1` and `debug = "line-tables-only"` do not change the deployed bytes. On v1.51 and v1.52, debug info changes p-token's code.
3. **A local rebuild does not reproduce the Docker build.** p-token, local macOS vs `solana-verify` Docker, both on v1.57, v3, `debug = 1`: the same size, sections and instruction count, but 481 `call` targets differ (the functions are in a different order). So labels from a rebuild are not safe, even with the same toolchain.
4. **`solana-verify build` (0.5.2)** mounts the repo and builds in place. The unstripped binary stays in `target/sbpf-solana-solana/release/<lib>.so` (V0) or `target/sbpfv3-solana-solana/release/<lib>.so` (v3). `--arch v3 --cargo-build-sbf-args="--tools-version v1.57"` works. Without those flags it picks the platform-tools of the Solana version in `Cargo.lock` (Solana 3.0 gives v1.51, V0).
5. **`anchor build --verifiable`** builds in the container with `CARGO_TARGET_DIR=/workdir/docker-target` on the host mount, copies only `deploy/<name>.so` to `target/verifiable/`, and then deletes `docker-target/`. The unstripped binary is lost. The command it runs inside the container is `cargo build-sbf --tools-version <v> --arch <a> --manifest-path <program Cargo.toml>`.
6. **Anchor 2.0.0-rc.1** always uses platform-tools v1.52 and V0. Its default image `solanafoundation/anchor:v2.0.0-rc.1` does not exist; `quay.io/ottersec/anchor:v2.0.0-rc.1` works. anchor-next (2.0.0-rc.2) defaults to v1.57 and v3 and has `--tools-version` / `--arch` flags.
7. **The images are amd64 only.** On Apple Silicon they need `--platform linux/amd64` (emulation, a few minutes per build). Each image is about 5 GB.
8. **Program hash.** `solana-verify` hashes the program as SHA-256 after it strips trailing zero bytes (`get_binary_hash`). This is not the same as a plain SHA-256 of the file. `verify --check-upstream` today uses a plain SHA-256 of `solana program dump` output (`verify/upstream_check.rs`, `format_hash`).
9. **`anchor verify` is `solana-verify`.** In Anchor 0.32.x and 2.0 rc, `anchor verify` runs `solana-verify verify-from-repo` and passes extra arguments through. It does not use Anchor's own Docker build. Anchor 0.31.1 and older do not: their `anchor verify` rebuilds with Anchor's own Docker flow.
10. **`solana-verify build` works on an Anchor workspace.** Tested on an Anchor 2.0.0-rc.1 program with `--arch v3 --cargo-build-sbf-args="--tools-version v1.57"`: the unstripped binary is kept, its `.text` matches the deploy `.so`, and it has full debug info.

## Goals

- One command, `qedgen build`, that the agent runs before `qedgen verify`. It produces the deploy `.so`, the unstripped `.so` and a build record, all from one Docker build.
- The build is reproducible with standard tools: `solana-verify verify-from-repo` with the recorded settings gives the same program hash. For Anchor 0.32+ this is also what `anchor verify` runs (finding 9).
- The build is tied to a git commit.
- A check that fetches the deployed program from chain and flags a hash mismatch against the build record.

## Non-goals

- Using the labels. That is qedsvm#81 and #82. #450 connects them.
- Deploying. The author deploys.
- sBPF assembly programs (`.s` sources). They are not built by `cargo build-sbf`.
- An upstream Anchor change to keep the unstripped binary. We may contribute it later, but this design must not depend on it.
- Reproducing Anchor's own `anchor build --verifiable` output. We do not use that flow (see Backend).
- Managing the `solana-verify` install. qedgen calls the one on `PATH` and records its version.

## Anti-goals

These would make the feature a failure even if it "works":

- **Labels from a different build than the deployed bytes.** No local-rebuild fallback, and no "close enough" match. If the unstripped `.text` does not match the deploy `.so`, stop.
- **A silent fallback when Docker is missing.** Refuse with a clear message instead.
- **A stamp that changes after deploy.** A deploy mismatch is a finding. It does not touch the stamp or the evidence for our commit.
- **A hash that does not match the ecosystem tools.** The recorded hash must be the same number `solana-verify` prints, so the author and third parties can check it.

## Design

### Command

`qedgen build` (new subcommand in `cli.rs`, dispatched in `run.rs`). The agent runs it. It is not a user-facing step in SKILL.md beyond "the agent builds before verifying".

Inputs (exact flag names are the builder's call):

- the program crate (same `--program` convention as `stamp` / `verify`),
- the library name, if it is not obvious from `Cargo.toml`,
- optional platform-tools version and arch overrides.

Defaults (our call): platform-tools v1.57, arch v3. Reason: debug info does not change the code on v1.57 (finding 2), and v3 is the project default for sBPF.

### Preconditions

Refuse, with a message that says how to fix it, when:

- the working tree is dirty (staged, unstaged or untracked files under the program and workspace). The stamp is tied to a commit, so the build must be too;
- Docker is not available or not running;
- `[profile.release]` in the workspace `Cargo.toml` has no `debug` setting. The message tells the author to add `debug = 1` (or `"line-tables-only"`) and commit. qedgen does not edit `Cargo.toml` itself, because the setting must be in the committed source that a verifier rebuilds. (Our call. The builder may instead offer to write it and stop for a commit.)

### Backend

**One backend for every cargo-built program (Anchor, Pinocchio, native, Quasar): call `solana-verify build`.**

- `solana-verify build --library-name <lib> --arch <a> --cargo-build-sbf-args="--tools-version <v>"`, with `DOCKER_DEFAULT_PLATFORM=linux/amd64` when the host is not amd64.
- Take the deploy `.so` from `target/deploy/<lib>.so` and the unstripped binary from `target/<sbpf dir>/release/<lib>.so` (`sbpf-solana-solana` for V0, `sbpfv3-solana-solana` for v3).
- This follows the project rule to call upstream tools rather than reimplement them.

Why not Anchor's own `anchor build --verifiable`: it deletes the unstripped binary (finding 5), and it is not what checks Anchor programs on chain. `anchor verify` in 0.32+ runs `solana-verify verify-from-repo` (finding 9). So building with `solana-verify` is the build that `anchor verify` reproduces, by construction. Finding 10 shows it works on an Anchor workspace. Because of this, there is no Anchor-specific path and no framework detection in the build. (The reviewer noted that `Framework::detect_from_cwd` in `verify/ratchet.rs` falls back to Anchor when it finds no marker; this design does not use it.)

Anchor 0.31.1 and older: their `anchor verify` uses Anchor's own Docker build, so it will not reproduce our hash. `solana-verify verify-from-repo` still will. `qedgen build` still runs, and the report says to verify with `solana-verify` directly. (Our call.)

Quasar: assumed to work, since it builds with `cargo build-sbf`. The builder confirms this against a Quasar example. If it does not work, the command refuses Quasar with a message.

`solana-verify` version: require at least 0.5.2 (the tested version), record the version in the build record, and refuse with an install hint when it is missing. qedgen does not install or pin it. (Our call.)

The record also stores the exact `solana-verify verify-from-repo` arguments that reproduce the build (`--library-name`, `--arch`, `--cargo-build-sbf-args`, commit), so the report can print a ready-to-run verify command. For Anchor 0.32+, the same arguments can be passed through `anchor verify`.

### Self-check (every backend)

After the build, before anything is written to the record:

- Extract `.text` from both files and compare bytes. If they differ, stop with an error. (Finding 1 says this should never fire. If it does, the labels would be wrong.)
- Confirm the unstripped binary has `.debug_info` and `.debug_line`. If not, stop: the `debug` setting did not apply.
- Confirm the ELF is the requested sBPF version. `verify/sbpf_elf.rs` already reads this.

Reading ELF sections should use an existing Rust ELF crate, not a shell-out to `llvm-objcopy`. The spike used `llvm-objcopy` only because it was at hand.

### Outputs

Written under `.qed/build/` in the project (exact layout is the builder's call):

- `<lib>.so`: the stripped deploy binary. This is the file we give to the author.
- `<lib>.debug.so`: the unstripped binary (label source for qedsvm#81).
- `build.json`, the build record:
  - `commit`: the full git commit SHA;
  - `program_hash`: `solana-verify` style (SHA-256 after trailing zero bytes are stripped), as the hex string that `solana-verify` prints;
  - `file_sha256`: plain SHA-256 of the deploy file, for humans who check with `shasum`;
  - `solana_verify_version`;
  - `image`: the image `solana-verify` used, with its resolved digest where Docker can report it;
  - `platform_tools`, `arch`, the exact build command line, and the matching `verify-from-repo` arguments;
  - `debug_setting`: the `[profile.release] debug` value that was in effect;
  - `text_matches: true` (the self-check result);
  - schema version and a timestamp.

`.qed/build/` holds binaries. Whether it is committed or git-ignored is the builder's call. I suggest git-ignored, with `build.json` small enough to commit if the user wants a record in git.

### Link to verify and stamp

- `VerifyEvidence` (`verify/evidence.rs`) gets a new optional field holding the build record's `commit` and program hash. It needs a new name: `program_hash` already exists there and holds the source digest. This is a schema version bump. Old records without it still load.
- `qedgen verify` reads `.qed/build/build.json` when present. If the record's commit is not the current `HEAD`, or the tree is dirty, it warns that the build is stale and does not copy it into the evidence.
- `qedgen stamp` keeps its current gates (spec hash, program source hash, implementation-verified). The build record is shown in the stamp report as "verified binary: <program_hash> at <commit>". Whether a missing build record should block stamping is left open in this version. Today's stamp flow (Anchor, source-level) must keep working without a build. (Our call: do not block, so this is additive in 2.x.)

### Deploy check

A new check that fetches the deployed program and compares it with the build record:

- Reuse the `solana program dump` fetcher in `verify/upstream_check.rs` (today it only checks CPI callees pinned in `qed.lock`).
- Hash the fetched bytes the `solana-verify` way (strip trailing zeros, then SHA-256). Without this, a correct deploy can look like a mismatch.
- A match is reported as OK. A mismatch is a finding with the two hashes and the commit. A program that is not deployed is reported as such, not as a failure.
- It never changes the stamp, the evidence or the build record.

Surface: a flag on `qedgen verify` next to `--check-upstream`, taking the program id and an optional RPC URL. (Our call. The exact name is the builder's.)

Separate question for the builder: `--check-upstream` uses a plain SHA-256 today. If callee pins were made from `solana program dump` output with zero padding, the plain hash and the `solana-verify` hash differ. Do not change `--check-upstream` behaviour in this work. Note it as a follow-up issue.

## Testing

- Unit tests for the record, the hash function (with and without trailing zeros), the dirty-tree and missing-`debug` refusals, and the evidence schema bump.
- The `.text` self-check, with a test where the two files differ.
- A Docker-backed integration test, `#[ignore]`, that builds one small Anchor fixture and one Pinocchio fixture, checks the self-check, and checks that `solana-verify`'s printed hash equals the recorded program hash. It runs by hand only for now, with no CI job. Each image is about 5 GB and emulated runs take minutes. A CI job (amd64 runner, image cache) is a follow-up issue. (Our call.)
- The deploy check with a fake fetcher (the `upstream_check` tests already have one).

## Documentation

- `references/cli.md`: the new command and the deploy-check flag.
- `docs/framework-support.md`: the build row per framework (Anchor, Pinocchio, native: `solana-verify`; Anchor 0.31 and older: `anchor verify` does not reproduce, use `solana-verify`; Quasar: per the builder's finding; sBPF: not supported).
- SKILL.md: one line in the verify flow saying the agent builds first. Not the dev-only details.
- Author note, for the report or README: set `debug` in `[profile.release]`. On platform-tools v1.57 it costs no code size or compute units. On older platform-tools it can change the code a little.

## Left to the builder

- Exact flag names, file names under `.qed/build/`, and JSON field names.
- Which ELF crate to use for the `.text` and section checks.
- Whether Quasar builds through `solana-verify` (report back).
- Confirm on the bundled Anchor 0.32.1 examples that `solana-verify build` works and `anchor verify` passes the recorded arguments through (report back).
- How to detect "dirty tree" precisely (whole repo vs workspace only).
