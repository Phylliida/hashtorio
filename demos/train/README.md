# the train (DESIGN-motion.md V1)

A moving structure built from existing primitives only — no engine changes.
Track = a loop of wires; the vehicle = a token circulating it; stations =
recipes. The timetable is the critical circuit: cycle = load(2) +
outbound(8) + unload(2) + return(13) = 25 ticks ⇒ exactly 1 ore delivered
per 25 ticks, first at t=13. The conservation audit shows the train (pulse)
neither injected nor delivered: conserved circulation. (The return leg is
13, not the Manhattan 12: since G0, placed belts are semantic, and the
track physically wraps around the unload dock — you can see the 27-cell
route do it.)

Run it:

```
cd demos/train
../../target/debug/gui 8471
# open http://127.0.0.1:8471 — press f to fit, watch the cyan token chug
```

The save file here is the fixture: it holds the track. The route-level
regression lives in `src/bin/gui/main.rs::a_train_circulates_and_delivers`
(one train ⇒ rate 1/25; two trains ⇒ 2/25 — fleet scaling is the (max,+)
theorem; transit assertions prove the token physically rides both track
segments).
