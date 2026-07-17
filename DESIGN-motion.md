# hashtorio — motion (design)

Can the game have *moving* structures — things that move themselves, not just
items riding belts? Pinned from conversation 2026-07-16. The answer is yes,
and the pleasing part is what it costs: **zero new primitives, one cache
rule, one editor sugar.** The kernel was already big enough for a moving
world; this document records why, and where the boundary sits.

## The principle

> **Finite modulo symmetry ⇒ summarizable.**

The steady-state theorem never demanded that *positions* be static; it
demands that the *description* be eventually periodic. A moving structure is
something periodic in a diagonal symmetry: translate it by `v·p` in space and
shift it by `p` in time and it maps to itself. That is exactly how HashLife
represents gliders — motion is not an exception to memoization, it is its
poster child, because a uniformly moving structure is a fixed point of
(translate ∘ time-shift).

The engine already exploits both halves separately:

- **Time-invariance** — staggered instances share memo entries (M3; the
  20k-instances-2-evals test).
- **Translation-invariance** — absolute position never enters the compiled
  net: wire latency is a Manhattan *difference* (`draft.rs::wire_latency`)
  and footprint collision is relative, so a translated factory interns to
  the *same NetId* and shares every cache entry. Pinned by the test
  `translation_yields_the_same_net_id` (which also checks the converse:
  stretching a wire is a different net).

Motion is the diagonal of two symmetries we already have. Physics footnote
that falls out for free: `BELT_SPEED` is a speed of light (nothing — item,
signal, or structure moving by means of the medium — outruns the causal cone
of the space-time diagram), and the Zeno refusal is the
no-closed-timelike-curves rule. Static factories are matter at rest; gliders
are the photons.

## Identity: bodies and souls

The One Law's **no item identity** (multiset semantics) is load-bearing for
cachability, and it dictates the two legal shapes of a moving thing:

1. **A stateless token** — indistinguishable from its siblings. Unlimited
   fleets, cheap. A train that is just "a train" is a chassis-shaped token
   circulating a track loop.
2. **A stateful node with a moving avatar** — a vehicle carrying internal
   state (a factory-on-wheels with work in progress) keeps its *soul* as an
   ordinary fixed node in the netlist; only its *body* — a position token —
   moves through space. They rendezvous through gating: the soul's ports
   admit flow exactly in the phase windows when the body is docked.
   Physically it moves; topologically it is stationary.

A fleet of stateful vehicles is `k` souls with `k` circulating bodies — a
*static* `k`, which is precisely the boundedness that keeps compilation
decidable. The conservation audit extends to fleets for free: bodies are
tokens, and tokens are conserved.

## Bounded motion is already derivable (zero new primitives)

Applying the standing minimality discipline ("is X derivable? which fragment
does it break?") to every rung — all of these live in the existing kernel
(recipe + marking + wiring, plus the already-admitted priority select):

| Component | Derivation |
|---|---|
| Track | a loop of wires (latency = length, as all wires) |
| Vehicle (stateless) | a chassis-shaped token circulating the track |
| Station | recipe `cargo + vehicle-here → loaded-vehicle @ load-time` |
| Switch / schedule | priority gates consuming route tokens |
| Docking window | else-gate whose token input is a clock's pulse stream |
| Shuttle arm / piston | clock-routed priority over k fixed-latency paths |
| Mobile factory | sealed module (soul) + circulating token (body) + windowed ports |

Two honest notes from the derivations:

- **Windows are admission counts, not durations.** In multiset semantics
  "port open for 3 ticks" is really "admits k items per cycle" — one clock
  pulse grants one passage. This is the more truthful notion anyway.
- **Time-varying latency is derived by time-multiplexing** (k parallel paths
  at k× part cost), not by making latency a function of time. Making it
  evaluator-native is possible — composing ultimately periodic countings with
  ultimately periodic delays stays in the class, periods lcm — but per the
  minimality rule it is *not admitted* unless the k-path derivation proves
  too costly in practice.

Consequence: trains, shuttles, pistons, and docking mobile factories are
expressible **today**, engine untouched. What is missing is only the idiom —
an editor "vehicle/track" piece that compiles away exactly as Belt does
(M9's precedent: the piece is sugar; the kernel never hears about it).

## The one genuinely new thing: a cache rule, not a primitive

Unbounded motion — crawlers, gliders, growing frontiers — visits infinitely
many world-states, but all translates of finitely many. The addition lives
entirely in the world/instance layer, where memoization already lives:

> **Motion summary rule.** If the world-state at `t+p` is a translate (by
> `v·p`) of the world-state at `t`, the stretch between is a motion summary
> `(net, displacement v, period p)` — close the loop and repeat it.

One cycle-detection rule over hash-consed world-states, made sound by the
translation-invariance pin above. No new node kinds, no new algebra: each
stretch is made of ordinary compiled nets, so every (min,+) theorem is
untouched. It is HashLife's memo rule imported into the layer that already
holds `World` and the `ChunkCache`. Operationally, a world-step between
stretches is a *scheduled retool* — and the live-edit machinery is already a
clock-preserving, marking-conserving retool, so the mechanism exists; what is
new is only recognizing recurrence modulo translation and closing the loop.

## The endgame unification: placement is slow flow

M11 made machines matter — a machine IS a chassis structure you own. The
minimal completion of that thought: let the *same recipe primitive* act on
chassis-tokens sitting in cell-places. Then a machine at cell `c` is a token
that moves rarely; an item is a token that moves often; **one substance, two
timescales**. "Moving structure" stops being a feature: it is a recipe whose
reactants include the machine itself. Self-motion is self-application (and
self-replication is adjacent — von Neumann's constructor with a conservation
audit, which M11 already gestures at).

The granularity objection ("every ground cell becomes a node") dissolves
against existing machinery: all ground cells are *one* hash-consed module
instantiated N times, and summaries key on `(NetId, input flows)`, so every
empty cell with zero inflow shares **one memo entry** — empty space evaluates
once, everywhere, forever. Cost concentrates on the frontier of activity.
Sparse infinite space falls out of the Library; this is HashLife-with-economy,
and it is the research-grade rung, not a feature rung: it needs lazy space
and a real design pass of its own.

## Relocation: machines moving on the grid, without stopping

Distinct from tokens-on-tracks: here the *machinery itself* — including
nested modules — gets shuffled around the grid, live. Each placement is a
distinct compiled net (adjacency + distance = wiring + latency), so a move
is a **seam** between nets. The design question is what crosses the seam,
and the answer is: everything, exactly. A running machine has no hidden
state — all of it derives from counting maps, and all of it transfers:

| Seam state | Carried as |
|---|---|
| port queues (supplied − consumed) | markings of the new net (the state primitive, used as intended) |
| items in flight on resized wires | scheduled arrivals — transient countings (positions are already exact; recompute remaining travel) |
| work in progress (fired, due at t+d) | scheduled productions, same mechanism |
| clock phases, priority reserves | special cases of the above |

> **The relocation law.** Anything may move at any time; the seam is always
> exact. Sealing doesn't *permit* moving — it shrinks the seam to the port
> buffers (the summary is position-free, per the translation-invariance
> pin). Boxes make moves cheap, never required.

(A first draft of this design said "power down to move, or seal" — that was
implementation caution misread as semantics, and it is retracted. The dual
principle "power-down to open" in DESIGN.md still stands: opening is
summary-to-microstate hydration; moving is not.)

Consequences:

- **The conservation audit must balance across seams** — a move can neither
  mint nor destroy items, including the ones mid-air. That is the test
  obligation that keeps relocation honest.
- **Doppler falls out.** A machine receding from its supplier receives at a
  slightly lower rate while in motion; approaching, higher. The piecewise
  algebra produces this without being asked.
- **Two deferred threads merge.** A periodic shuffle schedule is a mode
  automaton over pre-compiled placements — the input-regime-parametric
  summaries deferred since M5 and this document's motion-summary cache rule
  are the *same evaluator feature*. O(1) frames survive: stretch location is
  O(log seams), O(1) under a periodic schedule.
- **The delta from today is one word.** Live editing already relocates
  machinery with *rewrite-history* semantics (recompile from t=0, keep the
  clock). Relocation-as-mechanic is the upgrade to *seam-preserving*
  semantics: `evaluate_from(net, inputs, seam, t0)`, where a seam is just
  markings + transient countings — plumbing into machinery that exists.

Relocation rungs (meet the V-ladder at V4):

- **G0** ✅: grid-primary world — placed belts are semantic. `route.rs`
  (deterministic, translation-covariant A*; L-path fallback when hemmed)
  routes every wire at compile; latency and belt cost are the *routed*
  length; the scene carries the paths so the GUI draws exactly what you
  paid for. Migration honesty: repricing broke two tuned loops — the
  demo's demand clock (its self-loop rounds its own chassis: 5 cells =
  the whole 1/2 period, recipe latency now 0) and the train's return leg
  (24 → 25). Space got truer and the balance moved, same as M10.
- **G1** ✅: seam-preserving moves. A position-only edit is a relocation:
  the server opens a new epoch at the view tick, and the old epoch's exact
  state crosses the seam — queues as phantom constant inputs, in-flight and
  in-progress cohorts as scheduled arrivals (all phantom net inputs; the
  kernel is untouched), module state by prehistory concatenation
  (`Counting::suffix`/`concat` + an evaluator hook that replays a module's
  input history in front of its new flows — memo-sound because the memo
  keys on the concatenated inputs). Frames and harvest stitch across the
  epoch timeline; topology edits collapse it (rewrite semantics, as ever).
  Proofs: `a_move_is_a_seam_not_a_rewrite` (an iron conservation ledger
  reconstructed from stitched frames alone closes exactly across the seam)
  and `a_seam_preserves_module_state_exactly` (a factory with a stateful
  module matches its never-moved twin tick for tick, forever — the
  prehistory theorem). Honest costs: prehistory replay length ≈ elapsed
  ticks (late seams compile slower); carried in-flight cohorts ride
  phantom schedules, invisible to per-wire transit for a few ticks
  (cosmetic); interior drill views are epoch-local; the timeline is
  session-lived (restore begins a fresh epoch).
- **G2** ✅: **movers** — machines that move machines (`DraftNode::Mover`,
  the crane). Proofs: `a_mover_moves_a_machine_on_schedule` (the iron
  ledger closes across an *autonomous* seam; no-op firings open no
  epochs), `mover_unrolling_is_request_order_independent` (identical
  stitched totals and epoch counts however frames are requested — this
  test caught a real bug: an epoch's phantom flows leaked into the next
  seam's input vector, and not-yet-arrived phantom cohorts now re-carry
  across rapid successive seams), and `a_machine_that_walks` (a
  self-targeting mover walks three stops east on a token drip, footsteps
  at t=1, 35, 68 — not 1, 32, 62 — because its own fuel line stretches
  behind it: Doppler, predicted above, measured by the test). Design as
  pinned:

  - **To the kernel, a mover is an ordinary recipe** `1·token → 1·done @
    latency`. It never learns that firing means motion. The reflection is
    entirely the world layer *reading the mover's firing map* — and since
    counting maps are total functions, the world knows every future move
    at compile time. Reflection here is not a runtime callback; it is
    **lazy epoch unrolling**: find the earliest mover firing in the
    current epoch, advance that mover's target to its next stop, open an
    ordinary G1 seam there (state crosses exactly), recompile, repeat —
    on demand, as frames/harvest reach for later ticks. Deterministic and
    request-order independent (test-pinned).
  - **A mover is a player-hand made of tokens.** Its firing edits the
    draft's `node_pos` exactly as a drag does; the draft is the world's
    living blueprint and *evolves*. The client watches a `gen` counter
    and refetches when the factory rearranged itself.
  - **Configuration is static, timing is data.** `target` (any node —
    itself included) + `stops` (a cyclic list of placements; a per-mover
    cursor advances one stop per firing; simultaneous multi-fires
    coalesce, advancing the cursor by the count). Destinations chosen
    *by data* would be else-power beyond tier 1 — refused by
    construction, same boundary as ever.
  - **Self-targeting is legal and is the crawler seed**: a machine that
    moves itself walks, and its own fuel line stretches behind it
    (Doppler on its own supply).
  - **The done-pulse chains**: wire a mover's output to another mover's
    token port and you have choreography — rearrangement sequences.
  - **Failure = stall, life continues.** If a move can't compile
    (footprint overlap from interacting movers, no steady state), the
    firing is still consumed, the cursor still advances (self-healing:
    blocked stops get skipped next cycle), placement stays, and a status
    note surfaces. No wedged timelines. No-op moves (stop == current
    placement) open no epoch at all.
  - **Honest bounds (v1):** epoch cap with a visible note (recurrence
    summarization is V4's job); belt *capital* under mover-evolved
    layouts may transiently exceed owned stock — visible in the UI, not
    gated (deploy-time gating checks the drafted placement; worst-case
    reserve across stop combinations is future polish); the timeline
    remains session-lived.
- **G3** ✅: gradual motion. A mover firing no longer teleports its target —
  it schedules a **trundle**: one cell per `latency` ticks (the crane's
  pace knob; Zeno-guarded ≥1), each step an ordinary one-cell seam,
  **re-pathed by the router every step** with obstacles dilated by the
  machine's own bounding box, so it walks its whole body around whatever
  stands there *today*. Blocked walks stall gracefully and the next firing
  re-orders. Rendering is time-true: the scene carries a **placements
  timeline** (per-epoch positions), and the client renders machines where
  they *stood* at the view tick — belts re-route to match via the G0
  endpoint guard. Two bugs earned their tests: the wire router's eastbound
  seed + no-U-turn rule meant a walker could never take its first step
  west (it spiraled at its own doorstep — `route_free` seeds all four
  headings), and the walker's Doppler signature moved from footfalls to
  walk-starts (t = 2, 36, 69). Honest cost: a walk is one epoch per cell.
- **V2** ✅ *(realized the relocation way)*: mobile factories are **sealed
  modules carried by cranes** — `demos/rover/`, a commuting workshop that
  produces through three full commutes with the books balanced
  (`a_workshop_commutes_while_working`). G1's prehistory theorem carries
  the interior bit-exactly; G2 schedules; G3 makes the journey physical.
  The original soul/body token idiom remains the right pattern for one
  case only: fleets that must move at *belt speed* (tokens fly at c;
  machines trundle) — kept on the ladder as V2-token, unbuilt until
  needed.

## The boundary (unchanged, sharpened)

Aperiodic, data-dependent motion — a rover choosing turns from sensor
tokens — cannot live in the summarizable tier, for the same reason arbitrary
computation cannot: choices are else-power. That is not a new wall; motion
*inherits* the decidability boundary rather than moving it, and tier 2
(delta-state stepping + chunk cache, M5) handles the aperiodic case exactly
as it handles aperiodic flows. The caching-tier motto extends cleanly:

> Everything may move, provided the motion has a symmetry — a world with a
> published timetable.

## Related mathematics (pointers)

- **Timed event graphs / (max,+)**: vehicles-as-tokens is the textbook
  application — Heidergott, Olsder, van der Woude, *Max Plus at Work* is
  literally about railway timetables; throughput = critical cycle mean,
  which the evaluator already computes (M2's critical-circuit law test).
- **HashLife**: motion as spacetime periodicity; memoization quotiented by
  (translate ∘ time-shift); sparse space via shared empty summaries.
- **Petri nets**: bodies are conserved tokens; fleets audit for free.
- From the spatialization thread (world view, 2026-07-16): finite belt
  capacity / backpressure derives via a backward arc with marking = capacity
  and stays (min,+)-linear — that belongs to placed-belts P2, but it means
  jams and queues coexist with everything above.

## Roadmap

- **V0** ✅: pin translation-invariance
  (`draft.rs::translation_yields_the_same_net_id`).
- **V1** ✅: the train — built from existing primitives only (track loop,
  two station recipes, one circulating token; `demos/train/`). The
  timetable held on first contact: cycle 24 ⇒ 1 ore per 24 ticks, first
  delivery t=13, audit closes with the train conserved; two trains ⇒ 1/12
  (fleet scaling = the (max,+) theorem). Regression:
  `gui::tests::a_train_circulates_and_delivers`. Still open: the editor
  vehicle/track sugar that compiles to exactly this. *(G0 postscript: the
  cycle is now 25 — the return track physically wraps the unload dock.)*
- **V2**: mobile factories — soul/body idiom (module + clock-windowed ports),
  plus GUI rendering of the body token as the module's chassis moving along
  its track.
- **V3**: shuttle/piston idiom via time-multiplexing; revisit evaluator-native
  time-varying latency only if the derivation's k× cost hurts in practice.
- **V4**: the motion-summary cache rule — scheduled retools + recurrence
  detection modulo translation; a crawler as the demo (same net, displacement
  v, period p).
- **V5** (research): cell-lattice ground, lazy space, HashLife-with-economy;
  placement as slow flow.
