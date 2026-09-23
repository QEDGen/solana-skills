# Pinocchio `AccountInfo` scaffold contract

QEDGen's impl-targeted Pinocchio Kani harness cannot construct an
`AccountInfo` through a public constructor in Pinocchio 0.8.4. The only
supported runtime construction path is entrypoint deserialization, whose
large wire buffer and loop make these bounded proofs impractical. The shared
scaffold therefore uses one narrowly scoped unsafe representation conversion.

## Version and consumers

Generated Pinocchio crates pin `pinocchio = "=0.8.4"` and
`pinocchio-pubkey = "=0.2.4"`. The latter depends on Pinocchio 0.8; its 0.3
line depends on Pinocchio 0.9 and must not enter this dependency graph. Crates
that emit SPL Token layouts also pin `pinocchio-token = "=0.3.0"`. These exact
versions are part of the scaffold's safety contract, not merely reproducibility
choices.

The shared source is
`crates/qedgen/templates/kani-impl-pinocchio-scaffold.rs`. It is included by
`crates/qedgen/src/codegen/kani_impl/pinocchio.rs` and emitted into generated
`src/kani_impl.rs` files. The `ptoken-transfer` fixture is the hand-maintained
reference harness, while `kani-profile-diversity` and the generated snapshot
exercise the general emitter.

## Audited Pinocchio 0.8.4 contract

In Pinocchio 0.8.4, private `account_info::Account` is `#[repr(C)]`, 88 bytes,
and aligned to 8 bytes. Its fields are at these byte offsets:

| Field | Offset | Size |
| --- | ---: | ---: |
| `borrow_state` | 0 | 1 |
| `is_signer` | 1 | 1 |
| `is_writable` | 2 | 1 |
| `executable` | 3 | 1 |
| `original_data_len` | 4 | 4 |
| `key` | 8 | 32 |
| `owner` | 40 | 32 |
| `lamports` | 72 | 8 |
| `data_len` | 80 | 8 |

`AccountInfo` is a `#[repr(C)]` wrapper containing one `*mut Account`.
Pinocchio locates account data immediately after the 88-byte header. Its
entrypoint changes the runtime's `0xff` non-duplicate marker to `0`; in the
public borrowing API, set bits mean active borrows. A synthetic fresh account
must therefore initialize `borrow_state` to `0`, not `0xff`.

`AccountInfo::realloc` may expose up to `MAX_PERMITTED_DATA_INCREASE` (10,240)
bytes beyond the original data length. Every stack account reserves that full
zeroed region. This keeps the safe realloc API inside the backing allocation,
including the exact maximum-growth boundary.

The template checks header size, alignment, every field offset, the
`AccountInfo` pointer-wrapper size/alignment, and header/data contiguity at
compile time. The raw pointer is derived from the complete `StackAccount`, not
from a reference to its header field, so its provenance covers the trailing
data and realloc region. The unsafe helper additionally requires the stack
allocation to outlive all `AccountInfo` uses and forbids direct stack access
while a Pinocchio borrow guard is live. Generated harnesses meet this by
creating all stack accounts before their `AccountInfo` array and keeping them
in the proof function's scope.

## Data offsets and bounds

The SPL Token 0.3.0 token and mint offsets are fixed within exact 165-byte and
82-byte buffers. ABI-profiled records compute their total length by advancing
the schema offset for every field and bounded repeat; generated reads and
writes use Rust array indexing, so an inconsistent profile fails the harness
instead of accessing memory unchecked.

The integration test
`crates/qedgen/tests/pinocchio_account_info_scaffold.rs` includes the shipping
template itself and validates real Pinocchio accessors and both data/lamports
borrow trackers, real `pinocchio-token` token/mint parsing, zero-length data,
mutation, and both the exact permitted realloc boundary and its one-byte-over
rejection. Generated compile smokes inspect Cargo metadata and reject any
resolved Pinocchio version other than 0.8.4.
