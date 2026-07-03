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

**M0** — `Counting` (ultimately periodic counting maps in canonical form) with
the kernel op algebra (`add`/merge, `min`/join, `scale_floor`/gearing,
`shift`/latency) and `Recipe` application for feedforward nets. Canonical
normalization means semantic equality is structural equality is hash equality:

```rust
use hashtorio::Counting;

let belt = Counting::unit_rate();          // 1 item/tick
let geared = belt.scale_floor(2, 3);       // a 2:3 gearbox
assert_eq!(geared.rate(), (2, 3));         // exact rational rate

// same behavior, different construction -> same cache key
assert_eq!(belt.scale_floor(2, 1).scale_floor(1, 2), belt);
```

```
cargo test
```

Next up (M1/M2): the typed wiring term language with hash-consing, then
feedback via Kahn/(min,+) fixed points — at which point buffers, clocks, and
sensors emerge as derived components.
