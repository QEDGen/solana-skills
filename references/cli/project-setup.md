# CLI: project setup

`init`, `setup`, `asm2lean` — starting a project and installing toolchains.

Part of the [CLI Reference](../cli.md).

## Project setup

### `init`
Scaffold a new formal verification project. Creates `.qed/` project state
directory and pins the spec path in `.qed/config.json` so subsequent
commands don't need `--spec`.

```bash
$QEDGEN init --name escrow   --spec escrow.qedspec
$QEDGEN init --name tree     --spec tree.qedspec --asm src/tree.s
$QEDGEN init --name engine   --spec engine.qedspec --mathlib
$QEDGEN init --name counter  --spec counter.qedspec --target anchor
```

| Flag | Type | Default | Description |
|---|---|---|---|
| `--name` | String | required | Project name (alphanumeric + underscores) |
| `--spec` | Path | - | Spec path (file or directory) — written into `.qed/config.json` so `check`/`codegen` can resolve it automatically |
| `--asm` | Path | - | sBPF assembly source (runs asm2lean automatically) |
| `--mathlib` | bool | false | Include Mathlib dependency |
| `--target` | enum | - | Also generate the program crate + Kani harnesses for the named framework target. Values: `anchor` (Anchor-compatible Rust), `quasar` (Blueshift Quasar — `#![no_std]`, explicit discriminators, `Ctx<X>`), `pinocchio` (Pinocchio `#![no_std]` — `entrypoint!` + byte-discriminant dispatch, zeropod zero-copy state, `&AccountInfo` account structs with `.handler()` methods). Requires `--spec`. Omit to skip program scaffolding entirely. |
| `--output-dir` | Path | `./formal_verification` | Output directory |

The written `.qed/config.json`:

```json
{
  "name": "escrow",
  "spec": "escrow.qedspec",
  "interfaces_dir": ".qed/interfaces"
}
```

### `setup`
Set up the global validation workspace at `~/.qedgen/workspace/`.

```bash
$QEDGEN setup
$QEDGEN setup --mathlib
```

| Flag | Type | Default | Description |
|---|---|---|---|
| `--workspace` | Path | `~/.qedgen/workspace/` | Override workspace path |
| `--mathlib` | bool | false | Fetch Mathlib cache (~8GB) |

### `asm2lean`
Transpile sBPF assembly to Lean 4 program module.

```bash
$QEDGEN asm2lean --input src/program.s --output formal_verification/Prog.lean
```

| Flag | Type | Default | Description |
|---|---|---|---|
| `--input` | Path | required | sBPF assembly source file |
| `--output` | Path | required | Output Lean 4 file |
| `--namespace` | String | derived from filename | Lean namespace |

