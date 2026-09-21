# Installation and prerequisites

The existing repository and skill names remain unchanged:

```sh
npx skills add qedgen/solana-skills
```

Fresh installations keep the same command and behavior. The repository now
stores each runtime skill in its own directory so installing `qedgen` no longer
copies development-only source, fixtures, examples, or benchmark corpora.

## Upgrade an existing installation

Skills CLI 1.7.0 and newer can follow the `qedgen` skill from its former root
location to `skills/qedgen/` during an ordinary update. Use the same agent and
installation scope as the existing installation.

Skills CLI 1.5.9 treats the former root path as deleted during `skills update`.
Upgrade the Skills CLI, or explicitly re-add the named skill from the same
project and scope:

```sh
npx skills add qedgen/solana-skills --skill qedgen
```

If you invoke the Skills CLI directly, the tested non-interactive equivalent is
`skills add qedgen/solana-skills --skill qedgen --agent <agent> --yes`.

An update or re-add replaces the installed skill directory. That deliberately
removes development-only files and also removes the locally installed
`bin/qedgen`, which is installation state rather than bundled skill content.
After replacement, enter the installed `qedgen` skill directory and run the
installer again:

```sh
bash install.sh
tools/qedgen --help
```

The wrapper does not search `PATH`, download a binary, or build from source.
It only executes `bin/qedgen` inside the installed skill.

The optional Claude Code auditor thinking-budget hook is no longer stored in
or installed with `qedgen-auditor`. If an older installation enabled that hook,
remove or disable the old settings entry before replacing the skill. To keep
using it, install the source adapter into stable user-owned storage by following
the [auditor hook adapter guide](https://github.com/QEDGen/solana-skills/blob/main/integrations/qedgen-auditor-hooks/README.md)
from a source checkout. Ordinary skill installation never enables the hook.

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
