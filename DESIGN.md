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
- **M6** ✅: `render.rs` + the `hashtorio` binary. The renderer owns no
  state: every visual quantity — wire occupancy (supply − consumed),
  machine activity (firing deltas), delivered totals — is an O(1) `eval` of
  the counting maps, so random access in time is free and the presentation
  layer *cannot* desynchronize from the engine. `cargo run` opens a
  playground factory (gear line → overflow gate → demand store with live
  level gauge) with its published spec and conservation audit, and a REPL:
  step, `run N` (animated), `warp T` — warping to tick 1,000,000 is instant
  and exact, which is the whole thesis in one command.

- **M6.5** ✅: the browser GUI (`cargo run --bin gui`). A zero-dependency
  `std::net` HTTP server (hand-rolled JSON, ~no parsing: GET only) feeding
  a single-page vanilla-JS canvas app (`gui/index.html`, embedded via
  `include_str!`). The engine remains the sole owner of truth: the browser
  fetches the scene topology once and then batches of per-tick frames —
  each an O(1) read — and animates items along edges, queue bars, firing
  flashes, and output counters between ticks. Auto layered layout handles
  self-loops and the store's recirculation back-edge. Scrubber + warp box +
  a dedicated "warp 1,000,000" button. Frontend logic is validated
  headlessly in CI-fashion (node harness over real server JSON) since the
  repo has no browser test rig.

- **M7** ✅: editing. `draft.rs` is the editable blueprint model —
  deliberately dumber than a `Net` (flat lists in construction order, so
  editor positions map 1:1 onto compiled indices), compiled through
  `Draft::build` with *friendly* refusals: the compile button talks to a
  player, and the errors are the game teaching its own rules ("items can't
  be copied — split with a recipe", "add latency somewhere in that loop").
  The GUI gains an edit mode: parts palette (sources, machines via a recipe
  mini-language `2 iron -> 1 gear @3`, else-gates, outputs), port-to-port
  wire drawing with client-side linearity/type checks, markings (preloads),
  node dragging, deletion with index remapping, localStorage drafts, and
  `POST /api/compile` (server got a ~200-line std-only JSON parser). On
  success the new factory becomes the live scene — spec, audit, animation;
  on refusal the current factory survives. The demo itself is now a
  `Draft`, so "load current into editor" is exact. End-to-end validated
  headlessly: node drives the real editor functions (simulated port
  clicks) against the live server.

- **M8** ✅: module sealing — the recursion primitive in the player's
  hands. `DraftNode::Module` nests sub-drafts **by value** in the editor
  (one JSON blob, localStorage-friendly); at compile, sub-drafts intern
  first, so identical sealed modules dedup to one `NetId` — by-value in the
  editor, content-addressed in the engine. The GUI gains a select tool and
  seal/unseal: boundary wires become ports automatically (outer sources →
  module inputs, interior outputs feeding outside → module outputs),
  interior wires and markings move inside, and unseal splices everything
  back. The compiled view is strictly parent-level via
  `Evaluator::evaluate_detailed`: a sealed module renders as one node with
  port flows and *null* interior occupancy — the interior isn't hidden by
  the renderer, it is absent from the data, because the evaluator answered
  from the module's memoized summary. The abstraction boundary is real,
  not cosmetic. Cycles crossing a module boundary refuse kindly ("seal the
  whole loop inside, or keep it outside") — a GUI-view restriction, not an
  engine one (the engine flattens such cycles fine); lifting it means
  instrumenting flatten with an index correspondence. The demo now ships
  with its demand store sealed, and a regression test pins that sealing
  preserved every rate.

- **M9** ✅: structures instead of numbers — the game is situated in
  (product-)space. The move that preserves every cache guarantee:
  **structure lives in the type, not the item.** `structure.rs` interns 2D
  cell-sets (materials on a grid, origin-anchored, hash-consed exactly like
  nets); an `ItemType` now indexes this library, the eight historic types
  becoming single-cell primitives. Equality is *extensional* — any assembly
  route to the same shape yields the same id, cache entries, goal credit.
  Constructors: `weld` (union at an offset; refuses if cells collide) and
  `rot`. Polymorphic **builder** machines (weld/rot/split/belt) get their
  concrete types by forward inference at compile — the wiring graph is the
  expression tree of the artifact it builds — then compile to ordinary
  recipes, so the counting kernel is UNCHANGED: rates and shapes are
  orthogonal by construction (which is also why type-parametric modules
  are free even though rate-parametric summaries were deferred). New
  friendly refusals: parts collide; two shapes merged on one port; a
  builder in a type loop ("a structure cannot be built out of itself").
  **Machine-types are structures too**: every machine kind has an interned
  chassis, drawn as its icon, and the first manufacturing goal is the
  welder's own chassis — the demo factory welds iron+copper into a bar,
  splits, rotates one arm, welds the L, and thereby manufactures the
  machine that built it (goal MET at 1/2 per tick, regression-pinned).
  GUI renders shapes everywhere: items on wires, output ports, the goal
  panel.

- **M10** ✅: factory-space — distance is time, chassis are footprints.
  Drafts carry grid positions (empty = old abstract mode, so nothing prior
  broke); each wire compiles into an identity belt recipe with latency =
  Manhattan distance / BELT_SPEED — M9's polymorphic Belt was the missing
  piece, kernel unchanged again. Machine footprints are the chassis cells;
  placement collision is the same check as welding. Self-loops necessarily
  have length, so feedback has physical extent (Zeno = geometry). The demo
  immediately taught the lesson: its clock's feedback wire gained length,
  demand slowed, the store diverged, the summarizer refused — space
  changed game balance and had to be rebalanced. Frames carry per-wire
  transit (departed − arrived, exact); the GUI strings items physically
  along wires with latency labels, snapping, footprint outlines.
- **M11** ✅: the self-hosting economy. The pivotal identity: **a machine
  IS a chassis structure you own** — no separate machine entity. Inventory
  is structure counts; compiling checks blueprint cost against it
  recursively (module interiors included): "not enough weld chassis: this
  blueprint places 10, you own 3 — manufacture more." The other currency
  is the **tick budget**: watching and warping stay free (they simulate
  the blueprint — the whole engine thesis preserved), but **harvest**
  commits budget and banks output structures at exact eval differences,
  O(1) in harvested ticks. The factory keeps an internal clock across
  harvests; recompiling resets it (retooling loses work in progress —
  redesign has a real price). Meeting the manufacturing goal grants
  budget, once. The loop closes: harvest chassis → own more welders →
  place bigger factories → manufacture more chassis. Von Neumann's
  constructor, with a conservation audit.

- **M12** ✅: persistence, full costs, and enter-a-module editing.
  *Persistence:* the economy (inventory, budget, line clock, goal flag),
  the structure library (replayed on load so ids stay stable), and the
  current factory draft save to `hashtorio_save.json` (atomic tmp+rename)
  after every state change; corrupt saves move aside to `.bad` and the
  game starts fresh. *Costs:* wires cost one belt segment per tick of
  latency (a belt chassis is two cells long; BELT_SPEED is two cells per
  tick — the arithmetic is physical); markings are consumables — real
  items placed on the line, deducted at every compile, so retooling
  spends the preloads again; machines and belts remain reusable capital
  (need ≤ own). *Enter-a-module:* double-click a module in the editor to
  edit its interior in place — each nesting level carries its own grid
  positions (pocket dimensions with geometry), a breadcrumb shows the
  path, and editing a module's ports edits its parent's interface (parent
  wires to a deleted port are removed, higher legs shift down).
  Amusingly, the E2E initially "failed" because persistence worked too
  well: a test server restored a previous test's edited save.

- **M13** ✅ (G0 of DESIGN-motion.md): placed belts are semantic. Every
  wire routes through the grid at compile (`route.rs`: deterministic,
  translation-covariant A* around chassis footprints, turn penalty,
  L-path fallback when hemmed); the routed length *is* the latency and
  the belt cost, and the scene carries the paths so the GUI draws exactly
  what the player paid for. Repricing immediately re-taught the M10
  lesson twice: the demo's demand clock self-loop must round its own
  chassis (5 cells = the whole 1/2 period — recipe latency dropped to 0),
  and the train's return track wraps its unload dock (cycle 24 → 25).
  Saves whose loops were tuned to Manhattan fictions may refuse on
  restore (preserved as `.bad`, economy intact) — space got truer.

**Beyond M13 (future):** input-regime-parametric summaries (deferred from
M5); richer constructor algebra (transmutation rules, 3D cells); goal
ladders and progression; multiple save slots; WASM build.

**Motion** — moving structures (vehicles, mobile factories, crawlers) cost
zero new primitives: see `DESIGN-motion.md` (finite-modulo-symmetry
principle, body/soul identity split, the motion-summary cache rule).

## Implementation decisions

- Plain Rust for now, zero dependencies; the kernel algebra is kept pure and
  small so it can later be ported to / verified in Verus or tactus if we want
  the "the engine is a theorem" flex. Counts are `u64` with loud overflow
  panics (revisit with `u128`/bigint when real factories demand it).
