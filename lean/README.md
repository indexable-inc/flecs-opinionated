# Proofs of the merge rules

`merge/` is the Rust: plain functions over a small concrete value model, with no traits and no generics, so a
host, TS or a script sees the same rules as data (`Rule::{LastWriter, AddWins, Counter, KeepBoth}`). This directory
translates it to Lean 4 with [Charon](https://github.com/AeneasVerif/charon) and
[Aeneas](https://github.com/AeneasVerif/aeneas), and proves the laws on that translation, not on a hand-written copy.

```
uv run lean/regen.py           # translate merge/, lake build, write merge/tests/vectors.json, cargo test -p merge
uv run lean/regen.py --check   # the same, failing if a generated file differs from the commit (CI)
```

`Merge/Types.lean` and `Merge/Funs.lean` are generated; don't edit them. Run regen.py after any change to
`merge/src/lib.rs` and commit what it writes. It needs `lake` (elan), `rustup` and `cargo`; run it on a dev host.

## What is proven

Everything below is proven with no `sorry`; `regen.py` fails the build if one appears.

| Claim | Where |
|---|---|
| `merge r` is a join-semilattice on well-formed states, for every rule: it succeeds, and is commutative, associative and idempotent | `Merge/Laws.lean`: `merge_join` (from `merge_lastwriter_join`, `merge_addwins_join`, `merge_counter_join`, `merge_keepboth_join`) |
| A struct merged member by member (`merge_fields`) is a join-semilattice for any list of member rules: the field-wise product preserves the laws | `Merge/Laws.lean`: `merge_fields_join` |
| `validate` accepts only well-formed states | `Merge/Laws.lean`: `validate_sound` |
| Two accepted ops commute on any well-formed state, with the same result or the same error (`Overflow`) | `Merge/Ops.lean`: `apply_comm` (counter cases: `counts_bump_comm`, `inc_inc`, `dec_dec`, `inc_dec`) |
| An accepted op keeps a state well-formed, or fails with `Overflow` | `Merge/Ops.lean`: `apply_wf` |
| `hlc_lt` is a strict total order (irreflexive, transitive, asymmetric, total on distinct stamps; actor is the tie-break) | `Merge/Hlc.lean`: `hlc_lt_irrefl`, `hlc_lt_trans`, `hlc_lt_asymm`, `hlc_lt_total`, via `hlc_lt_spec` |
| `hlc_send` and `hlc_recv` move the clock past the local stamp (and the remote one), never behind `now`, or report counter overflow | `Merge/Hlc.lean`: `hlc_send_spec`, `hlc_recv_spec` |

How the proofs go. `Merge/Spec.lean` is a model: a finite map as a list sorted strictly by key, with `Nat` values
joined by `max` (a set is a map whose values are all 0). Its `union` is commutative, associative and idempotent by
extensionality: a sorted list is determined by its lookups, and lookup through `union` is the pointwise max.
`Merge/Refine.lean` shows each generated list function (`stamps_union`, `tagged_union`, `counts_union`,
`counts_bump`) returns `ok` with exactly the model's result, and that the abstraction is injective, so equal models
are equal Rust values. The laws in `Laws.lean` then transfer. `LastWriter` is `max` under the key
(stamp, value), which is injective, so ties resolve the same on every replica.

Scope of the claims: "well-formed" means each list is sorted strictly by key, as `validate` checks; states built by
`apply` from the empty state stay well-formed (`apply_wf`). An op is "accepted" by a rule as in `Op.fits` (Counter
takes `Inc`/`Dec`, AddWins `Add`/`Remove`, KeepBoth `Put`, LastWriter `Write`). Ops are prepared effectors: an
`Add` carries a fresh tag and a `Remove` the tags it saw, so their commutation needs no causal delivery. That tags
are unique (one HLC stamp per add) is the caller's job, as in every OR-set; the clock laws above are what make
stamps from `hlc_send` unique per actor. There is no network model: convergence of replicas follows from the
semilattice laws (state-based) and `apply_comm` (op-based), as in the CRDT literature.

## Test vectors

`lake exe vectors` (`Vectors.lean`) runs the generated, proven Lean functions on random well-formed inputs and
writes `merge/tests/vectors.json`: rule, inputs and expected output for `merge`, `apply` and `merge_fields`,
including mismatched shapes and counter overflow. `merge/tests/vectors.rs` replays them against the Rust. A TS
merge can replay the same file; the shapes are at the top of `Vectors.lean`. Integers are u64 and can pass 2^53,
so parse them as big integers in JS.

## Pinned versions

| Tool | Version | Pinned in |
|---|---|---|
| Aeneas | release `nightly-2026.10.24-74aabd5` (commit 74aabd52f77001cf9eaad51b4f4b867a7dc6b111), tarball sha256 checked | `regen.py` (`AENEAS_TAG`, `AENEAS_REV`, `SHA256`), `lakefile.toml` (Lean library rev) |
| Charon | 0.1.294 (commit 260ba2c82588291b1f9efaf4f42d7100adc9c6c2), bundled with that Aeneas release | `regen.py` (`CHARON_VERSION`, checked) |
| Charon's rustc | nightly-2026-09-17, read from the release's `rust-toolchain` | the Aeneas release |
| Lean | v4.35.0 | `lean-toolchain` |
| Mathlib | v4.35.0 (56a005855ea17e0b0958f73ec801ae140b0a1679), through Aeneas | `lake-manifest.json` |

To move to a newer Aeneas, change `AENEAS_TAG`, `AENEAS_REV`, `CHARON_VERSION` and the hash in `regen.py`, the
rev in `lakefile.toml`, and `lean-toolchain` to the Lean that Aeneas's `backends/lean/lean-toolchain` names, then
`lake update` and rerun regen.py. Aeneas nightly releases are not kept forever; if the tag is gone, move up.

## Prior art and what was reused

- Gomes, Kleppmann, Mulligan and Beresford, "Verifying strong eventual consistency in distributed systems"
  (OOPSLA 2017, [arXiv 1707.01747](https://arxiv.org/abs/1707.01747)), Isabelle/HOL. Reused: the proof strategy
  for op-based CRDTs, convergence from the commutation of operations on a common state, and the OR-set's
  unique-tag argument. Their network model is not reproduced here.
- Shapiro, Preguiça, Baquero and Zawirski, "Conflict-free Replicated Data Types" (SSS 2011) and the
  comprehensive study (INRIA RR-7506): the state-based definitions (G-Counter, PN-Counter, OR-set, MVR) and the
  join-semilattice criterion.
- Kulkarni et al., "Logical Physical Clocks" (2014): the hybrid logical clock's send and receive rules.
- Lean 4 CRDT work found on GitHub, read for definitions and checked against ours, no code copied:
  [velvetmonkey/crdt-lean](https://github.com/velvetmonkey/crdt-lean) (Mathlib CvRDTs: G-Set, G-Counter,
  PN-Counter, OR-Set, RGA), [mlavrent/verified-crdt](https://github.com/mlavrent/verified-crdt),
  [nathanial/convergent](https://github.com/nathanial/convergent) (op-based CmRDTs). None of them proves a Rust
  implementation; no Aeneas-based CRDT project was found.

## Later: the move-safe tree

Kleppmann, Mulligan, Gomes and Beresford, "A highly-available move operation for replicated trees" (IEEE TPDS
2021), is out of scope here. To add it:

1. Rust: a `Tree` value in `merge/` as a sorted list of (child, parent, metadata) plus a log of `Move` ops ordered
   by HLC stamp, and an `apply` that undoes later ops, applies the move unless it would create a cycle, and redoes
   them. Keep the ancestor check a bounded recursion over the sorted list so Aeneas translates it.
2. Lean: a model of the tree as a relation, the paper's two lemmas (the tree stays a tree, no cycles; applying a
   log in any order of arrival gives the state of applying it in stamp order), and refinement lemmas as in
   `Refine.lean`. The paper's Isabelle/HOL proofs are the outline to follow.
3. Add `Rule::Tree` and its cases to `merge`, `apply`, `validate`, `Value.wf`, `merge_join` and the vectors.
