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
| Flow meter (tap) | recipe `A → A + pulse` — every passing item emits a pulse |
| Reservoir gauge (level ≥ K) | consume-and-refund loop `K·A → K·A + pulse`, latency ≥ 1 |
| Valve | join sensed pulses with the gated flow |

Honest limitation found while deriving these (M2): the reservoir gauge
*sequesters* its reservoir — single-sink wires mean the sensed items cannot
also drain downstream. Metering a through-flow is the tap; level-sensing a
*drainable* buffer needs tier-1 (priority) machinery. This is the Petri-net
zero-test friction showing up exactly where the theory says it must.
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

- **M0** ✅: `Counting` — ultimately periodic counting maps in canonical
  form, with the op algebra (shift, add/merge, min/join, floor-scaling,
  recipe application) and canonical hashing. Tested against naive dense
  evaluation.
- **M1** ✅: term language — typed wiring DAG (`net.rs`), hash-consed library
  (a Merkle DAG by construction), module flattening (`flatten.rs`).
- **M2** ✅: feedback (`eval.rs`) — SCC decomposition; acyclic nodes evaluate
  symbolically, module summaries memoized on `(NetId, input countings)`;
  cyclic components solved by **guess-then-verify**: dense simulation
  proposes an ultimately periodic candidate and exact M0 algebra verifies
  the fixed-point equations. Cycles have latency ≥ 1, so the causal solution
  is unique and any verified candidate is *the* behavior — soundness never
  rests on the guessing heuristic. Cycles through module boundaries trigger
  flattening. Divergence is refused honestly (`RateExplosion`,
  `NoPeriodicSteadyState`, `ZeroLatencyCycle`). Derived components in
  `components.rs`: clock, throughput throttle (the critical-circuit law
  `rate = min(input, tokens/latency)` holds as a test), reservoir gauge.
- **M3** ✅: `report.rs` — Summary (exact port rates + first arrivals: the
  cache entry as player-visible contract) and the conservation Audit
  (per-type ledger; no-conjuring checked pointwise-forever via `min`; books
  close as exact rationals). `world.rs` — the instance layer: instances are
  (design, start tick, local inputs); time-invariance means staggered
  instances share memo entries (20k instances, 2 interior evals, as a test).
- **M4** ✅: `Node::Priority` — the else, admitted deliberately. Inputs
  `[item, token]`, outputs `[granted, fallback]`: arriving items take a
  token and go left while tokens last, else go right, same tick.
  `priority.rs` derives its behavior on ultimately periodic inputs with a
  **closure certificate**: either an exact reserve-state repeat at equal
  input phase (deterministic recurrence ⇒ periodic forever) or a
  token-surplus argument (full grants with headroom over a full period +
  token rate ≥ item rate ⇒ full grants forever). Guess-then-verify extends
  unchanged: priority nodes verify by re-derivation from candidate inputs.
  Derived tier-1 components: overflow splitter, and the **demand store** —
  the drainable buffer with a live level gauge that M2's honest limitation
  note said was impossible in the kernel. Non-monotonicity is asserted in
  tests (more tokens ⇒ less overflow; more demand ⇒ fewer level pulses).
  *Scope honesty:* summaries remain exact per-(design, input flows) — the
  memo still caches them; a symbolic input-regime-parametric mode-automaton
  summary (one entry covering all input regimes) is future work (M5-era).
- **M5** ✅: tier 2 lives (`stepper.rs`). Nets the summarizer honestly
  refuses (breeders, growing pools) run exactly anyway, via **delta-state**
  stepping: per-wire slack, per-node recent firing deltas, per-priority
  token reserve — all *relative*, no absolute counts, so two instances of a
  design in the same `StepState` advance identically on identical input
  deltas. That soundly keys the `ChunkCache`:
  `(design, state, input chunk) → (state, output chunk)`, shared across
  instances and time even for never-periodic behavior. `World` is now
  tiered: symbolic summary first, stepper fallback on refusal, both behind
  one `output_count` API. Cross-validation test: stepper ≡ symbolic
  evaluator tick-for-tick on summarizable nets (two independent
  implementations of the semantics agreeing exactly).
  *Deferred with honesty:* input-regime-parametric mode-automaton summaries
  (one symbolic entry covering all input regimes) remain future work — the
  regime geometry deserves a design pass of its own, not a rushed slice.
- **M6**: world, rendering (semantics-free presentation layer).

## Implementation decisions

- Plain Rust for now, zero dependencies; the kernel algebra is kept pure and
  small so it can later be ported to / verified in Verus or tactus if we want
  the "the engine is a theorem" flex. Counts are `u64` with loud overflow
  panics (revisit with `u128`/bigint when real factories demand it).
