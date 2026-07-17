# the rover — a mobile factory (V2 by way of G1+G2+G3)

Run `../../target/debug/gui 8473` here and watch the world view: a sealed
workshop — a real factory with interior state, a 2-iron→1-gear press with
queues and work in progress — **commutes** between two posts, carried by a
crane on a patrol clock, cell by cell, while producing the whole time.

This is V2's "mobile factory" realized the relocation way. The original
soul/body sketch (a stationary module + a circulating body token +
clock-windowed ports) turned out to be unnecessary for this: G1's
prehistory theorem means a sealed module's interior state rides along
*bit-exactly* when the module is relocated, G2's crane provides the
schedule, and G3's trundling makes the journey physical — one-cell seams
at the crane's pace, re-pathed around obstacles each step.

What to look at:

- **It works while it walks.** Gears keep flowing at 1/2 through every
  commute; the conservation books balance across every one-cell seam
  (regression: `gui::tests::a_workshop_commutes_while_working` — three
  full commutes, 24 seams, totals monotone, no stall).
- **Doppler.** While the workshop walks away from the mine its supply
  arrivals thin; walking back, they bunch. The piecewise algebra does
  this unasked.
- **Time is honest.** Scrub backward: the workshop renders where it
  *stood* at that tick (the scene's placements timeline), and belts
  re-route to match.

The token-body idiom from the design doc remains the right pattern for
one case this doesn't cover: fleets that must move at *belt speed* —
tokens fly at c, machines trundle. For everything else, put the factory
in a box and carry the box.
