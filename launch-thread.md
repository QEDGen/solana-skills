# QEDGen Launch Thread

## Tweet 1 (Hook) [VIDEO ATTACHED]

If you're writing Solana programs in raw sBPF, there's no type system watching your back. No Anchor. No static analyzer. Just you and the instructions.

Just shipped a QEDGen update that formally verifies these programs end-to-end. Here's what's new:

## Tweet 2 (What's new)

Two big things.

asm2lean: feed it your .s file, it transpiles the assembly straight into Lean 4. No more hand-transcribing bytecode into proof models.

Aristotle (@harmonicfun): an agentic prover for the hard sub-goals. Leanstral fills easy gaps in seconds, Aristotle grinds through the walls.

## Tweet 3 (Result)

Ran it on a real sBPF program — branching validation, stack writes, pointer arithmetic, memory disjointness.

13 properties proven. Zero unproven assumptions. Auth, conservation, arithmetic safety, PDA integrity. 66 second build.

Not a demo.

## Tweet 4 (CTA)

It's MIT licensed, go nuts:

npx skills add qedgen

Point it at your sBPF program. It reads the assembly, writes specs, generates Lean 4 proofs, and iterates until they build.

github.com/QEDGen/qedgen
