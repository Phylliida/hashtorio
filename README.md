# hashtorio
tasty hash brows yum!

A Factorio-like built around one idea: machines contain machines, recursively,
and the semantics is **cachable by construction** so enormous factories run
cheaply. Wires carry cumulative counting functions (monotone staircases); the
kernel fragment is ultimately periodic, so every module summarizes to an exact
finite object that doubles as its cache key.

See [DESIGN.md](DESIGN.md) for the design (the one law, the kernel generators,
the derived standard library, and why the caching tier boundary is the
decidability boundary).

## Status

**M0–M4** — kernel algebra, term language, evaluator with feedback,
conservation audit + instance layer, and the tier-1 priority primitive:

- `Counting`: ultimately periodic counting maps in canonical form, with the
  kernel op algebra (`add`/merge, `min`/join, `scale_floor`/gearing,
  `shift`/latency). Canonical normalization means semantic equality is
  structural equality is hash equality.
- `Net`/`Library`: typed wiring terms, hash-consed into a Merkle-DAG
  blueprint library; item linearity is enforced structurally (merge is free,
  copying doesn't exist).
- `Evaluator`: module summaries memoized on `(design, input flows)` — the
  HashLife move — and feedback loops solved by guess-then-verify: simulation
  proposes an ultimately periodic steady state, exact algebra verifies the
  fixed-point equations. The critical-circuit law (loop throughput =
  tokens/latency) falls out as a passing test, and divergent nets (breeder
  loops) are refused honestly rather than mis-summarized.
- `report`/`world`: the summary as player-visible spec (exact rates, first
  arrivals); a conservation audit that proves no wire is ever over-consumed
  and closes the per-type books as exact rationals; and the instance layer —
  20,000 staggered instances of two designs cost two interior evaluations.
- `Priority` (tier 1): the consciously-admitted *else* — items take a token
  and go left while tokens last, else right. Evaluated exactly via closure
  certificates; buys overflow splitters and drainable demand-stores with
  live level gauges, at the price the theory demands (non-monotone, bigger
  cache keys). Tests assert the non-monotonicity on purpose.

```rust
use hashtorio::Counting;

let belt = Counting::unit_rate();          // 1 item/tick
let geared = belt.scale_floor(2, 3);       // a 2:3 gearbox
assert_eq!(geared.rate(), (2, 3));         // exact rational rate

// same behavior, different construction -> same cache key
assert_eq!(belt.scale_floor(2, 1).scale_floor(1, 2), belt);
```

```
cargo test           # 47 tests, exact semantics cross-validated two ways
cargo run            # terminal playground: step, run, warp a live factory
cargo run --bin gui  # browser GUI at http://127.0.0.1:8470 (canvas, animated)
```

The GUI is a zero-dependency std::net HTTP server feeding a single-page
vanilla-JS canvas app: animated item flows, live queue bars, firing
machines, the published spec and conservation audit alongside — plus a
play/pause/speed bar, a scrubber, and a **warp** box. Try the
`warp 1,000,000` button: the frame at tick one million renders instantly
and exactly, because every frame — drawn or warped to — is an O(1) read of
the counting maps, never a simulation. That one button is the thesis.

Next up (M1/M2): the typed wiring term language with hash-consing, then
feedback via Kahn/(min,+) fixed points — at which point buffers, clocks, and
sensors emerge as derived components.
