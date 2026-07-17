# the crab — a glider (V4.2)

Run `../../target/debug/gui 8474` here. One node, one wire, one marking:

- a **mover targeting itself** with a relative gait (`"stops":[[1,0]],
  "relative":true`) — each firing, it walks one cell east of wherever it
  stands;
- its **done-pulse wired into its own token port** (done type == token
  type), so it is its own clock — the engine rides onboard, no anchored
  fuel line, no Doppler drag;
- a single token marking to wake it.

It skitters east forever at one cell per four ticks. Within three strides
the recurrence detector notices that its translation-quotiented
fingerprint has recurred at a shifted anchor and closes the loop:
`cycle {t0: 6, period: 4, shift: [1,0]}`. Nothing is ever materialized
again — scrub to tick five million and the client renders the crab
exactly `(5,000,000 − 6) ÷ 4` cells east of home, by modular arithmetic.

This is recurrence modulo translation — HashLife's glider, in an economy
with conservation audits. Regression: `gui::tests::the_crab_glides_forever`.

Given as a redstone flying machine crab by Danielle, 2026-07-16, which
turned out to be the design document. Faces sideways, as is proper.
