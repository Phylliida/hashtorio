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
- **V1**: the train — built from existing primitives in the live game
  (track loop, two stations, cargo cycle), proof by working machine. Then,
  optionally, the editor vehicle/track sugar that compiles to exactly that.
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
