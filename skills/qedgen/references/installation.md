# Installation and prerequisites

The existing repository and skill names remain unchanged:

```sh
npx skills add qedgen/solana-skills
```

From the installed qedgen skill directory, explicitly install its CLI:

```sh
bash install.sh
tools/qedgen --help
```

The wrapper only runs an existing executable at `bin/qedgen`. If it is missing,
the wrapper exits with installation instructions. It never downloads or builds.

The installer downloads the skill's pinned release and SHA-256 checksum over
HTTPS, verifies the checksum and reported version, then atomically replaces
`bin/qedgen`. Failed verification preserves any existing binary and exits
nonzero. Checksums detect corruption or substitution relative to the release's
checksum file; they are not independent publisher signatures.

By default installation writes only inside the skill directory. No shell
configuration or global PATH links are changed. To request one link explicitly:

```sh
bash install.sh --link-dir "$HOME/.local/bin"
```

Ensure that chosen directory is on your PATH yourself. An unrelated file or
symlink at the destination is preserved and installation refuses the conflict.

A full source checkout can fall back to a locked Cargo build if its release
binary is unavailable. Unsupported platforms require explicit `--from-source`. Checksum, execution and
version failures do not fall back. Portable packages contain no Rust sources,
so download failure exits with instructions. To explicitly build a source checkout:

```sh
bash install.sh --from-source
```

Source builds use your installed Rust toolchain and may fetch Cargo dependencies.
QEDGen does not install Rust, Lean, Kani, or API keys. Provision prerequisites
yourself before requesting commands that need them:

- Rust for source builds and generated Rust tests.
- [Lean / elan](https://lean-lang.org/install/) for proofs. Provision the version
  selected by the workspace's `lean-toolchain`.
- [Kani](https://model-checking.github.io/kani/install-guide.html) for Kani verification.

Lean/Kani availability checks look for executables on PATH without running
toolchain-manager shims. The requested build checks whether those tools work.
`qedgen setup` prepares a Lean workspace and may fetch dependencies through
`lake update`; `--mathlib` also fetches Mathlib/cache. Subsequent proof commands
may prepare a missing workspace. Builds can fetch dependencies, and toolchain
managers such as elan can fetch an unprovisioned selected toolchain when invoked.
