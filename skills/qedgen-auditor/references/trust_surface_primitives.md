# Trust-surface primitives — per-class failure-mode checklists

Companion reference for SKILL.md §3c (Trust-surface dep walk). Read on
demand: only consult the section matching the primitive class the dep
implements. Don't read this file end-to-end every audit; that defeats
the purpose of keeping per-class knowledge out of the always-loaded
SKILL.

Each section has the same shape:

- **Class name and canonical references**
- **Stated security property** the program is leaning on
- **Failure modes** (numbered list, with the textbook attack name)
- **What to grep for in the dep's source** — the syntactic signal of
  the failure
- **Quick verdict criteria** — when to call it sound vs flag

If the dep's class isn't covered below, fall back to the first-
principles list in SKILL.md §3c step 2 and write up the new class so
the next audit has a checklist.

---

## Signature schemes

### Hash-based one-time signatures (Lamport, Winternitz, WOTS, WOTS+)

**Canonical refs:** Merkle 1979/1982 (original Winternitz), Hülsing
2013 (WOTS+), RFC 8391 (XMSS, embeds WOTS+ spec including §3.1.5
"Checksum"), Buchmann et al. 2007 ("Merkle Signatures with Virtually
Unlimited Signature Capacity").

**Stated property:** Existential unforgeability under chosen-message
attack (EU-CMA) given one observed signature per keypair.

**Failure modes:**

1. **Missing checksum / digit-domination forge.** Per-digit hash-chain
   signature without an appended `C = sum_i (w-1 - d_i)` digit string
   signed in its own chains. Attacker observes `sig` on `m1`, grinds
   `m2` with `digest(m2) ≤ digest(m1)` pointwise, walks each chain
   forward by `(d1[i] - d2[i])`. Polynomial-time, no hash-function
   break needed. (A WOTS-shape vault program in our audit corpus
   had a fired reproducer for exactly this.)
2. **Key reuse beyond one signature.** Even with checksum, a second
   signature with the same keypair reveals enough chain positions to
   forge any third message. The program must enforce one-shot
   consumption (atomic close, rotation marker, or PDA-seed binding).
3. **Hash function with insufficient preimage resistance.** Chain
   security reduces to `H` being one-way; truncation below ~2^128
   classical / ~2^64 quantum breaks the EU-CMA reduction.
4. **Parameter mismatch sign vs verify.** Different chain length `w`
   or digest length between signer and verifier silently over-accepts
   (verifier walks fewer steps than signer hashed back).
5. **Pubkey commitment not bound to keypair.** If the stored merkle
   root / pubkey hash isn't tied to the seed material (or to a PDA
   the program controls), an attacker can swap in a pubkey they hold
   the key for.

**Grep for in dep source:** `fn sign`, `fn verify`, `fn recover_pubkey`
in the WOTS-shape dep. Look for the loop that hashes the message
digest; count whether there's a second loop for checksum chunks. No
second loop = missing checksum.

**Verdict criteria:** Sound iff (1) checksum present, (2) one-shot
enforced at program layer, (3) hash function ≥128-bit security, (4)
signer/verifier use identical parameters, (5) pubkey commitment is a
PDA seed or stored field. Any missing → flag.

### Schnorr / EdDSA variants

**Canonical refs:** Schnorr 1990, RFC 8032 (Ed25519, Ed448), BIP-340
(Bitcoin Schnorr), FROST (threshold).

**Stated property:** EU-CMA in the random oracle model under
discrete-log assumption.

**Failure modes:**

1. **Nonce reuse.** Two signatures with the same nonce `k` leak the
   private key: `(s1 - s2) / (e1 - e2) = k`, then `x = (s1 - k*e1) /
   r1`. Look for non-deterministic-nonce code paths or per-call
   randomness without commitment to the message.
2. **Malleability.** Signature `(R, s)` accepts `(R, -s)` if the
   verifier doesn't enforce the canonical low-`s` form (or
   equivalent). BIP-340 fixes this; Ed25519 has the
   "small-subgroup point" variant.
3. **Domain-separation collisions.** Same key signing for two
   different applications without a domain-separator tag → cross-
   protocol replay. Look for the hash input to `e` (the challenge)
   — does it include a context tag?
4. **Cofactor / small-subgroup attacks.** Curve points not in the
   prime-order subgroup; multiplicand attacks. Check whether
   `verify` rejects low-order points.
5. **Batched verification soundness.** Batch verifiers that combine
   multiple signatures with random scalars must reject if any
   individual sig is invalid. Soundness depends on the random
   scalars being unpredictable to the signer.

**Grep for in dep source:** `nonce`, `k`, `rng`, `RngCore`, `seed`,
`scalar_mul`, `pre_hash` / `prehash` / `dom_sep`.

**Verdict criteria:** Sound iff deterministic nonce (RFC 6979) OR
hash-message-to-nonce; canonical sig form enforced; domain separator
present in challenge hash; subgroup check on `R`; batched paths
verified with explicit batch-soundness argument.

### BLS / pairing-based aggregable signatures

**Canonical refs:** Boneh-Lynn-Shacham 2001, IETF draft-irtf-cfrg-bls-
signature, RFC 9380 (hash-to-curve).

**Stated property:** EU-CMA under co-CDH / SXDH in pairing-friendly
groups; aggregation-secure under rogue-key attack with proof-of-
possession (PoP).

**Failure modes:**

1. **Rogue-key attack.** Without PoP, attacker registers pubkey
   `pk' = pk_target^(-1) * g^x`, then `aggregate(pk_target, pk') =
   g^x`, signing as `target`. Mitigation: PoP or
   proof-of-knowledge of secret key at registration.
2. **Hash-to-curve weakness.** Non-uniform hash-to-curve maps leak
   discrete-log information. Use RFC 9380 hash-to-curve only.
3. **Sub-group confusion.** Wrong G1/G2 group at verification.
   Verify groups match the protocol spec.
4. **Pairing-soundness on malformed inputs.** Some impls accept
   identity-point or non-canonical encodings; either trivially
   verify or panic.

**Grep for in dep source:** `aggregate`, `proof_of_possession`,
`hash_to_curve`, `G1Affine`, `G2Affine`, `pairing`, `miller_loop`.

**Verdict criteria:** Sound iff PoP enforced at key registration,
RFC 9380 hash-to-curve, canonical encoding rejected on malformed
points, correct group used at each step.

### Threshold signatures (FROST, GG18/20, etc.)

**Failure modes:** key-share leakage during DKG, biased nonce
distribution, missing identifiable abort under malicious behavior,
re-randomization issues across signing sessions. Highly protocol-
specific; if a Solana program uses one, audit the dep against the
specific protocol's known attacks (e.g., the GG20 vs GG18 fix).

---

## Commitment schemes

### Pedersen / vector Pedersen / KZG

**Stated property:** Computationally binding, perfectly hiding (or
the reverse for KZG-style); openings unique under DL assumption.

**Failure modes:**

1. **Trusted setup compromise.** KZG / Bulletproofs over Pedersen with
   a structured-reference-string (SRS). If the program holds the
   toxic waste, binding breaks.
2. **Binding-via-collision.** Generic-hash commitment `H(value ||
   randomness)` is only binding if `H` is collision-resistant —
   not preimage-resistant. Programs using truncated hashes (< 256
   bits) may be off the security parameter for binding.
3. **Hiding broken by structured randomness.** Randomness must be
   uniform over the commitment group. LCG / weak-RNG sources break
   hiding.
4. **Equivocation via algebraic structure.** Pedersen `g^v * h^r`
   binding rests on `log_g(h)` being unknown. If `h` is derived
   suspiciously (e.g., `h = g^c` for known `c`), binding is
   trivially broken.

**Verdict criteria:** Sound iff trusted setup is trusted, hash
function ≥256 bits for binding, randomness from a CSPRNG, basepoints
generated nothing-up-my-sleeve.

### Merkle trees / Merkle proofs

**Failure modes:**

1. **Second-preimage via length-extension or unequal-leaf-depth.** If
   leaf and internal-node hashes are domain-separated (`0x00 || leaf`
   vs `0x01 || node`), this is sound. If not, an internal node can
   be passed as a leaf. Common bug pattern.
2. **Non-canonical tree shape.** Variable-depth trees with no length
   pin allow proof-fraud (two distinct trees both verify against
   the same root).
3. **Hash function preimage gap.** Same threshold as Pedersen ≥256
   bits.
4. **Storing the bump / proof byte-order.** Solana-specific: bump
   byte must be a fixed position, otherwise off-by-one re-derivation
   creates collisions.

**Verdict criteria:** Sound iff domain-separated hash, canonical
encoding (RFC 6962 or equivalent), ≥256-bit hash output.

---

## ZK verifiers (Groth16, Plonk, etc.)

**Failure modes:**

1. **Verifying-key mismatch.** Hardcoded VK in the program differs
   from the circuit it was compiled against. Catastrophic — any
   proof verifies. Easy to introduce on circuit updates.
2. **Proof-malleability.** Verifier accepts `(A, B, C)` and also
   `(-A, B, -C)` for the same statement. Allows replay of "different"
   proofs that bind to the same witness.
3. **Public-input encoding mismatch.** Verifier and prover use
   different encodings for public inputs (endianness, modular
   reduction). Either silently rejects valid proofs (DoS) or
   silently accepts invalid ones (forge).
4. **Pairing-side reuse from BLS class above.** ZK over pairings
   inherits the BLS failure modes.
5. **Trusted-setup contamination.** Universal-setup ceremonies are
   only as secure as the most-trusted participant. If the dep ships
   a hardcoded SRS, audit its provenance.

**Verdict criteria:** Sound iff VK and circuit hash are committed
together (e.g., a content hash of the circuit pinned in the program),
canonical proof encoding enforced, public inputs domain-separated
and length-pinned, SRS provenance documented.

---

## VRFs (verifiable random functions)

**Stated property:** Pseudorandomness conditioned on a public-key
binding; pubkey-holder cannot bias output without breaking
underlying signature scheme.

**Failure modes:**

1. **Output bias / non-uniform sampling.** VRF output should be
   indistinguishable from uniform random conditioned on the input.
   Most-significant-bit bias is a tell.
2. **Replay across contexts without domain separator.** Same VRF
   key signing for two different applications without a `dom_sep` →
   cross-application output replay.
3. **Pubkey commitment leakage.** Pubkey must be committed before
   the input is known; otherwise the holder grinds the input to
   bias the output.

**Verdict criteria:** Sound iff output is hashed-to-uniform with a
strong hash, dom_sep present in input, pubkey committed at a stage
before the input is chosen.

---

## Custom hash constructions

If the dep implements a hash function or a hash-based construction
(Davies-Meyer, Merkle-Damgård variant, sponge, etc.) rather than
using a battle-tested one, the answer is usually "do not." Any
hand-rolled hash is a finding unless the audit reaches a verdict
that all of:

- Pre-image resistance under the target security parameter
- Second-preimage resistance under the target security parameter
- Collision resistance under the target security parameter
- Indifferentiability from a random oracle (if used in a sig scheme
  or commitment)

…hold against state-of-the-art attacks. This is essentially never
within audit scope; flag as CRIT with "use a standard hash" as the
fix unless the dep is a well-known construction implemented
correctly.

---

## How to add a new class

When you encounter a primitive class not listed here:

1. Audit the dep using first principles from SKILL.md §3c step 2.
2. After the audit, write up the class as a new section here using
   the template at the top of this file.
3. The next audit on the same class consults this section instead of
   re-doing the first-principles walk.

The point isn't to make this an encyclopedia; the point is that the
*process* (locate trust claim → list failure modes → verify against
canonical reference → flag deltas) is invariant. The
*per-class failure-mode lists* are the artifact the process produces.
This file is where those artifacts accumulate.
