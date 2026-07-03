//! The instance layer: many placed copies of few designs, one shared cache.
//!
//! An [`Instance`] is (design, start tick, input flows in instance-local
//! time). The semantics is time-invariant — a module born at tick `s`
//! behaves, relative to its birth, exactly like one born at tick 0 — so the
//! world evaluates every instance in local time and shifts at the read
//! boundary. Instances of the same design with the same local inputs share
//! one memo entry *regardless of start tick*: the flyweight. Per-instance
//! state is just `(NetId, u64, inputs)`; all the heavy summaries live in the
//! shared evaluator cache.

use crate::counting::Counting;
use crate::eval::{EvalError, Evaluator};
use crate::net::NetId;

#[derive(Debug, Clone)]
pub struct Instance {
    pub design: NetId,
    /// World tick at which this instance starts existing.
    pub start: u64,
    /// Input flows in instance-local time (tick 0 = birth).
    pub inputs: Vec<Counting>,
}

pub struct World<'l> {
    pub ev: Evaluator<'l>,
    instances: Vec<Instance>,
    outputs: Vec<Option<Vec<Counting>>>,
}

impl<'l> World<'l> {
    pub fn new(ev: Evaluator<'l>) -> Self {
        World { ev, instances: Vec::new(), outputs: Vec::new() }
    }

    pub fn spawn(&mut self, design: NetId, start: u64, inputs: Vec<Counting>) -> usize {
        self.instances.push(Instance { design, start, inputs });
        self.outputs.push(None);
        self.instances.len() - 1
    }

    pub fn len(&self) -> usize {
        self.instances.len()
    }

    pub fn is_empty(&self) -> bool {
        self.instances.is_empty()
    }

    /// Evaluate every instance. Identical (design, inputs) pairs hit the
    /// shared memo no matter how many instances exist or when they start.
    pub fn run(&mut self) -> Result<(), EvalError> {
        for (i, inst) in self.instances.iter().enumerate() {
            if self.outputs[i].is_none() {
                self.outputs[i] = Some(self.ev.evaluate(inst.design, &inst.inputs)?);
            }
        }
        Ok(())
    }

    /// Cumulative count on an instance output port at a world tick.
    pub fn output_count(&self, instance: usize, port: usize, world_tick: u64) -> u64 {
        let inst = &self.instances[instance];
        let outs = self.outputs[instance].as_ref().expect("run() first");
        if world_tick < inst.start {
            0
        } else {
            outs[port].eval(world_tick - inst.start)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::components::{clock, throttle};
    use crate::net::{ItemType, Library};

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

        // Shifted reads: a clock born at tick s pulses first at s + 5.
        assert_eq!(world.output_count(0, 0, 4), 0);
        assert_eq!(world.output_count(0, 0, 5), 1); // instance 0 starts at 0
        assert_eq!(world.output_count(2, 0, 5), 0); // instance 2 starts at 1
        assert_eq!(world.output_count(2, 0, 6), 1);

        // Two instances of the same design are pointwise time-translates.
        for t in 0..100 {
            assert_eq!(world.output_count(0, 0, t), world.output_count(2, 0, t + 1));
        }
    }
}
