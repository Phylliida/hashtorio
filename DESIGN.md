# hashtorio — design

A Factorio-like where machines contain machines, all the way down, and the
engine runs huge factories cheaply because the semantics is *built* to be
cachable. This document pins the design derived in conversation (2026-07-03).

## The one law

A machine's observable behavior is a pure function of
**(its design, its small instance state, the history at its ports)**.
Nothing else crosses the boundary. Composition is defined so that a wired-up
network of machines is itself a machine with the same interface type —
*closure under coupling* (cf. the DEVS theorem of the same name). Because a
box of machines summarizes to the same kind of object as a primitive machine,
summaries compose recursively and can be memoized at every level of nesting,
HashLife-style.

Everything below is forced by this law plus a minimality rule: a primitive is
admitted only if it is provably not derivable from the others.

## The semantic object

A wire carries, for each item type, a **cumulative counting function**
`N : tick → count` — a monotone staircase giving the total number of items
that have ever crossed. A machine is a causal monotone map between bundles of
counting functions.

Consequences (each one load-bearing):

- **No item identity, no ordering.** Semantics is multisets; nothing can
  depend on "which" item or FIFO order. Violating cachability is not
  disciplined against — it is inexpressible. (Rendering may still draw items
  marching down belts; that is presentation, not semantics.)
- **Merge is free and deterministic.** Two same-typed wires joining is
  pointwise `+` on counts. Commutative, so no arbitration primitive exists.
- **Feedback is well-defined.** Monotone + causal ⇒ Kahn least fixed points:
  every wiring diagram, loops included, has exactly one behavior.
- **Steady states are exact.** The monotone fragment is (min,+)-linear in the
  sense of *Synchronization and Linearity* (Baccelli, Cohen, Olsder, Quadrat):
  behavior is ultimately periodic with rational slopes determined by critical
  circuits. A module's cached summary is an exact transfer map, not an
  approximation. All arithmetic is exact (integers / rationals — never
  floats); cache keys must be exact.

## The kernel (generators)

1. **Recipe** — consume inputs at integer ratios, produce outputs at integer
   ratios, with latency `d`:
   `2·A + 3·B → 1·C + 1·D  after d ticks`.
   Semantics: firings `k(t) = min_i ⌊N_i(t−d) / c_i⌋`, output `j` counts
   `p_j · k(t)`. One operator carrying synchronization, rate change, and time.
2. **Initial tokens (marking)** — a wire may start preloaded with `k` items.
   The *only* state primitive.
3. **Wiring** — junctions, feedback, and typed wires (a wire declares which
   item types it accepts, so a mixed stream meeting typed wires dispatches by
   type automatically). This is structure, not machinery: tensor, sum, and
   trace of the ambient category.

A **module** is *not* a primitive: it is a named subterm. Blueprints are
hash-consed terms in a DAG; caching is memoizing denotations of subterms.

## The standard library (all derived)

| Component | Derivation |
|---|---|
| Belt | identity recipe with latency |
| Gearbox p:q | recipe `p·A → q·A` |
| Round-robin splitter | recipe `2·A → 1·A_left + 1·A_right` (counts split exactly) |
| Buffer, capacity K | feedback wire of "space" tokens, marking K; delivery refunds a space token |
| Clock, period d | self-loop recipe `tick → tick`, latency d, marking 1 |
| Sensor (level ≥ K) | consume-and-refund recipe `K·A → K·A + pulse` |
| Valve | join sensed pulses with the gated flow |
| Signals | ordinary token types that happen to be free to mint (resource economics, not new semantics) |

## The one thing that cannot be derived

"Take from A, **else** B." Anything with an *else* — priority, overflow,
filter-with-fallback — is non-monotone (more input on A means less output
from B) and provably absent from the kernel. If admitted, it is the fourth
primitive, **priority select**, and it is exactly inhibitor-arc power:

- Kernel without it ≈ Petri nets / timed event graphs: analyzable, summaries
  computable in closed form.
- With it: Turing-complete; perfect summaries impossible in general.

**The caching tier boundary is the decidability boundary:**

| Tier | Fragment | Summary | Cost |
|---|---|---|---|
| 0 | kernel only | exact (min,+) transfer map (ultimately periodic) | O(1) per event, forever |
| 1 | + priority select | mode automaton, event-driven | fast, larger key space |
| 2 | unbounded cleverness | memoized stepping (HashLife-style chunks) | still dedups across identical instances |

In-game part cost can mirror summary cost: the priority splitter is expensive
*because it is expensive*. The player-visible "spec" on a module honestly
degrades as they use more of it.

## Engine architecture notes

- **Blueprints are hash-consed terms.** Content-addressed (Merkle) design DAG;
  identical sub-designs share cache entries globally. Instance state is a tiny
  vector (markings/phase); design summaries are shared flyweights.
- **Conservation is exact.** Rational rates emit integer items via
  Bresenham-style accumulators, deterministically. Caching must never create
  or destroy an item.
- **Port buffers are phase firewalls.** Module ports carry small mandatory
  buffers so that only rates and latency are observable across a boundary —
  summaries compose in rate algebra, not exact-timing algebra. Inside a
  module you may choreograph; across boundaries you get a rate contract.
- **Power-down to open.** Opening a sealed (summarized) module requires it to
  spin down to a quiescent state first — deletes the summary-to-microstate
  hydration problem.
- **The summary is a spec.** "12/s iron in → 3/s gears out, latency 40 ticks"
  is shown to the player; modules are published by contract.

## Roadmap

- **M0** (this): `Counting` — ultimately periodic counting maps in canonical
  form, with the op algebra (shift, add/merge, min/join, floor-scaling,
  recipe application) and canonical hashing. Tested against naive dense
  evaluation.
- **M1**: term language — typed wiring DAG, hash-consing, evaluator for
  feedforward nets.
- **M2**: feedback — Kahn/(min,+) fixed points, markings, critical-circuit
  steady states; derive buffer/clock/sensor and test them.
- **M3**: module summaries + memo cache; conservation accounting.
- **M4**: priority select; mode-automaton tier.
- **M5**: tier-2 memoized stepping fallback.
- **M6**: instances, world, rendering (semantics-free presentation layer).

## Implementation decisions

- Plain Rust for now, zero dependencies; the kernel algebra is kept pure and
  small so it can later be ported to / verified in Verus or tactus if we want
  the "the engine is a theorem" flex. Counts are `u64` with loud overflow
  panics (revisit with `u128`/bigint when real factories demand it).
