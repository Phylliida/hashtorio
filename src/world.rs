//! The instance layer: many placed copies of few designs, one shared cache —
//! now with tiered execution.
//!
//! An [`Instance`] is (design, start tick, input flows in instance-local
//! time). The semantics is time-invariant — a module born at tick `s`
//! behaves, relative to its birth, exactly like one born at tick 0 — so the
//! world evaluates every instance in local time and shifts at the read
//! boundary. Instances of the same design with the same local inputs share
//! one memo entry *regardless of start tick*: the flyweight.
//!
//! **Tiering.** `run()` first asks the symbolic evaluator for an exact
//! ultimately-periodic summary (tiers 0/1: O(1) reads at any tick, shared
//! per design+inputs). If the summarizer honestly refuses — `RateExplosion`
//! (superlinear growth) or `NoPeriodicSteadyState` — the instance falls
//! back to tier 2: exact tick-stepping through the shared [`ChunkCache`],
//! where identical instances in identical states still share work,
//! chunk by chunk, even though no closed form exists.

use std::collections::HashMap;

use crate::counting::Counting;
use crate::eval::{EvalError, Evaluator};
use crate::net::NetId;
use crate::stepper::{ChunkCache, Stepper, CHUNK};

#[derive(Debug, Clone)]
pub struct Instance {
    pub design: NetId,
    /// World tick at which this instance starts existing.
    pub start: u64,
    /// Input flows in instance-local time (tick 0 = birth).
    pub inputs: Vec<Counting>,
}

enum Run {
    /// Tiers 0/1: exact closed form, O(1) random-access reads.
    Summarized(Vec<Counting>),
    /// Tier 2: exact stepping; `cum[t][port]` = cumulative output after
    /// local tick `t`.
    Stepped { stepper: Stepper, design_key: u32, cum: Vec<Vec<u64>> },
}

pub struct World<'l> {
    pub ev: Evaluator<'l>,
    pub chunks: ChunkCache,
    design_keys: HashMap<NetId, u32>,
    instances: Vec<Instance>,
    runs: Vec<Option<Run>>,
}

impl<'l> World<'l> {
    pub fn new(ev: Evaluator<'l>) -> Self {
        World {
            ev,
            chunks: ChunkCache::new(),
            design_keys: HashMap::new(),
            instances: Vec::new(),
            runs: Vec::new(),
        }
    }

    pub fn spawn(&mut self, design: NetId, start: u64, inputs: Vec<Counting>) -> usize {
        self.instances.push(Instance { design, start, inputs });
        self.runs.push(None);
        self.instances.len() - 1
    }

    pub fn len(&self) -> usize {
        self.instances.len()
    }

    pub fn is_empty(&self) -> bool {
        self.instances.is_empty()
    }

    /// Number of instances that fell back to tier-2 stepping.
    pub fn stepped_count(&self) -> usize {
        self.runs
            .iter()
            .filter(|r| matches!(r, Some(Run::Stepped { .. })))
            .count()
    }

    /// Evaluate every instance: symbolic summary where possible, stepper
    /// fallback where the summarizer honestly refuses.
    pub fn run(&mut self) -> Result<(), EvalError> {
        for i in 0..self.instances.len() {
            if self.runs[i].is_some() {
                continue;
            }
            let inst = &self.instances[i];
            self.runs[i] = Some(match self.ev.evaluate(inst.design, &inst.inputs) {
                Ok(outs) => Run::Summarized(outs),
                Err(EvalError::RateExplosion)
                | Err(EvalError::NoPeriodicSteadyState { .. }) => {
                    let next_key = self.design_keys.len() as u32;
                    let design_key =
                        *self.design_keys.entry(inst.design).or_insert(next_key);
                    Run::Stepped {
                        stepper: Stepper::new(self.ev.lib(), inst.design)?,
                        design_key,
                        cum: Vec::new(),
                    }
                }
                Err(e) => return Err(e),
            });
        }
        Ok(())
    }

    /// Cumulative count on an instance output port at a world tick.
    /// Summarized instances answer in O(1); stepped instances advance
    /// (through the shared chunk cache) as far as needed.
    pub fn output_count(
        &mut self,
        instance: usize,
        port: usize,
        world_tick: u64,
    ) -> Result<u64, EvalError> {
        let inst = self.instances[instance].clone();
        if world_tick < inst.start {
            return Ok(0);
        }
        let local = world_tick - inst.start;
        match self.runs[instance].as_mut().expect("run() first") {
            Run::Summarized(outs) => Ok(outs[port].eval(local)),
            Run::Stepped { stepper, design_key, cum } => {
                while (cum.len() as u64) <= local {
                    // Next chunk of input deltas, in instance-local time.
                    let t0 = stepper.tick();
                    let input_deltas: Vec<Vec<u64>> = (0..CHUNK as u64)
                        .map(|j| {
                            let t = t0 + j;
                            inst.inputs
                                .iter()
                                .map(|c| {
                                    c.eval(t) - if t == 0 { 0 } else { c.eval(t - 1) }
                                })
                                .collect()
                        })
                        .collect();
                    let outs =
                        self.chunks.advance(*design_key, stepper, &input_deltas)?;
                    for out in outs {
                        let mut last = cum
                            .last()
                            .cloned()
                            .unwrap_or_else(|| vec![0; out.len()]);
                        for (acc, d) in last.iter_mut().zip(&out) {
                            *acc += d;
                        }
                        cum.push(last);
                    }
                }
                Ok(cum[local as usize][port])
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::components::{clock, throttle};
    use crate::net::{ItemType, Library, NetBuilder};
    use crate::recipe::Recipe;

    const ITEM: ItemType = ItemType(0);
    const TOKEN: ItemType = ItemType(1);
    const PULSE: ItemType = ItemType(2);

    #[test]
    fn ten_thousand_instances_two_summaries() {
        let mut lib = Library::new();
        let clk = clock(&mut lib, 5, TOKEN, PULSE);
        let thr = throttle(&mut lib, ITEM, TOKEN, 3, 4);

        let mut world = World::new(Evaluator::new(&lib));
        for i in 0..10_000u64 {
            // Staggered starts; same designs, same local input flows.
            world.spawn(clk, i % 97, vec![]);
            world.spawn(thr, i % 89, vec![Counting::unit_rate()]);
        }
        world.run().unwrap();

        // 20,000 instances, exactly 2 interior evaluations: the flyweight.
        assert_eq!(world.len(), 20_000);
        assert_eq!(world.ev.interior_evals, 2);
        assert_eq!(world.stepped_count(), 0);

        // Shifted reads: a clock born at tick s pulses first at s + 5.
        assert_eq!(world.output_count(0, 0, 4).unwrap(), 0);
        assert_eq!(world.output_count(0, 0, 5).unwrap(), 1); // starts at 0
        assert_eq!(world.output_count(2, 0, 5).unwrap(), 0); // starts at 1
        assert_eq!(world.output_count(2, 0, 6).unwrap(), 1);

        // Two instances of the same design are pointwise time-translates.
        for t in 0..100 {
            assert_eq!(
                world.output_count(0, 0, t).unwrap(),
                world.output_count(2, 0, t + 1).unwrap()
            );
        }
    }

    #[test]
    fn breeders_fall_back_to_shared_stepping() {
        let mut lib = Library::new();
        // Exponential breeder with a pulse tap: unsummarizable, steppable.
        let breeder = {
            let mut b = NetBuilder::new();
            let n = b.recipe(
                Recipe::new(vec![3], vec![4, 1], 2),
                &[ITEM],
                &[ITEM, PULSE],
            );
            b.connect(n.output(0), n.input(0));
            b.marking(n.input(0), 3);
            let out = b.output(PULSE);
            b.connect(n.output(1), out);
            lib.intern(b.build()).unwrap()
        };
        let thr_id = throttle(&mut lib, ITEM, TOKEN, 3, 4);

        let mut world = World::new(Evaluator::new(&lib));
        let b1 = world.spawn(breeder, 0, vec![]);
        let b2 = world.spawn(breeder, 10, vec![]); // staggered twin
        let t1 = world.spawn(thr_id, 0, vec![Counting::unit_rate()]);
        world.run().unwrap();

        assert_eq!(world.stepped_count(), 2);
        // Symbolic and stepped instances coexist; the summarized one agrees
        // with an independent evaluation.
        let mut ev2 = Evaluator::new(&lib);
        let expect = ev2.evaluate(thr_id, &[Counting::unit_rate()]).unwrap()[0].eval(100);
        assert_eq!(world.output_count(t1, 0, 100).unwrap(), expect);

        // Advance the first breeder past one chunk; its staggered twin then
        // replays entirely from the shared chunk cache.
        let v1 = world.output_count(b1, 0, 60).unwrap();
        let misses_after_first = world.chunks.misses;
        let v2 = world.output_count(b2, 0, 70).unwrap();
        assert_eq!(v1, v2, "time-translates of the same design");
        assert_eq!(world.chunks.misses, misses_after_first, "twin fully cache-fed");
        assert!(world.chunks.hits >= 1);
        assert!(v1 > 1000, "exponential growth is really happening: {v1}");
    }
}
