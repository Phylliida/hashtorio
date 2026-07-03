//! Semantics-free presentation: draw a factory at any tick, from the
//! counting maps alone.
//!
//! Nothing here simulates. A [`Scene`] precomputes, per node input leg, the
//! wire's *supply* and *consumption* counting maps; every visual quantity
//! is then an O(1) `eval` at the requested tick:
//!
//! - wire occupancy (items queued on a belt) = supply(t) - consumed(t)
//! - machine activity = firing delta at t
//! - delivered totals = output counting maps
//!
//! Random access in time is free: rendering tick 1,000,000 costs the same
//! as tick 3. The renderer is a *reader* of the semantics, never an owner —
//! the presentation layer cannot desynchronize from the engine because it
//! has no state of its own.

use std::collections::HashMap;

use crate::counting::Counting;
use crate::eval::Trace;
use crate::net::{ItemType, Library, Node};

pub struct Scene {
    trace: Trace,
    labels: Vec<String>,
    out_labels: Vec<String>,
    type_names: HashMap<u32, String>,
    in_types: Vec<Vec<ItemType>>,
    /// Per node, per input leg: the wire's supply counting map.
    supplies: Vec<Vec<Counting>>,
    /// Per node, per input leg: the leg's consumption counting map.
    consumed: Vec<Vec<Counting>>,
}

impl Scene {
    /// `labels`: one per node of the (flattened) traced net.
    /// `out_labels`: one per net output.
    pub fn new(
        lib: &Library,
        trace: Trace,
        labels: Vec<String>,
        out_labels: Vec<String>,
        type_names: &[(ItemType, &str)],
    ) -> Scene {
        assert_eq!(labels.len(), trace.net.nodes.len(), "one label per node");
        assert_eq!(out_labels.len(), trace.net.outputs.len(), "one label per output");
        let layout = crate::net::Layout::new(lib, &trace.net);
        let mut supplies = Vec::new();
        let mut consumed = Vec::new();
        let mut in_types = Vec::new();
        for (n, node) in trace.net.nodes.iter().enumerate() {
            in_types.push(lib.node_in_types(node));
            let legs = lib.node_in_types(node).len();
            supplies.push(
                (0..legs)
                    .map(|l| {
                        let wire = &trace.net.wires[layout.node_input_wire(n, l)];
                        let mut acc = Counting::constant(wire.marking);
                        for src in &wire.sources {
                            let c = match src {
                                crate::net::Source::Input(i) => &trace.inputs[*i as usize],
                                crate::net::Source::NodeOut { node, leg } => {
                                    &trace.node_outs[*node as usize][*leg as usize]
                                }
                            };
                            acc = acc.add(c);
                        }
                        acc
                    })
                    .collect::<Vec<_>>(),
            );
            consumed.push(match node {
                Node::Recipe { recipe, .. } => recipe
                    .consume
                    .iter()
                    .map(|&c| trace.firings[n].scale_floor(c, 1))
                    .collect(),
                Node::Priority { .. } => vec![
                    trace.node_outs[n][0].add(&trace.node_outs[n][1]),
                    trace.firings[n].clone(),
                ],
                Node::Module(_) => unreachable!("scenes render flattened nets"),
            });
        }
        Scene {
            trace,
            labels,
            out_labels,
            type_names: type_names
                .iter()
                .map(|(ty, n)| (ty.0, n.to_string()))
                .collect(),
            in_types,
            supplies,
            consumed,
        }
    }

    /// Items queued on the wire feeding node `n`, leg `l`, at tick `t`.
    pub fn occupancy(&self, n: usize, l: usize, t: u64) -> u64 {
        self.supplies[n][l].eval(t) - self.consumed[n][l].eval(t)
    }

    /// Firings of node `n` at exactly tick `t`.
    pub fn fired(&self, n: usize, t: u64) -> u64 {
        let f = &self.trace.firings[n];
        f.eval(t) - if t == 0 { 0 } else { f.eval(t - 1) }
    }

    /// Cumulative deliveries on output `port` by tick `t`.
    pub fn delivered(&self, port: usize, t: u64) -> u64 {
        self.trace.outputs[port].eval(t)
    }

    fn type_name(&self, ty: ItemType) -> &str {
        self.type_names.get(&ty.0).map(|s| s.as_str()).unwrap_or("?")
    }

    /// One dashboard frame. Pure function of `t`.
    pub fn frame(&self, t: u64) -> String {
        let mut s = format!("tick {t}\n");
        for (n, label) in self.labels.iter().enumerate() {
            let act = self.fired(n, t);
            let marker = if act > 0 { format!("*{act}") } else { "  ".into() };
            s.push_str(&format!("  {label:<22} {marker:<4}|"));
            for l in 0..self.supplies[n].len() {
                let occ = self.occupancy(n, l, t);
                let bar = "#".repeat(occ.min(10) as usize);
                s.push_str(&format!(
                    " {}:{bar}{}",
                    self.type_name(self.in_types[n][l]),
                    occ
                ));
            }
            s.push('\n');
        }
        s.push_str("  --\n");
        for (o, label) in self.out_labels.iter().enumerate() {
            let total = self.delivered(o, t);
            let delta = total - if t == 0 { 0 } else { self.delivered(o, t - 1) };
            s.push_str(&format!("  {label:<22} {total}"));
            if delta > 0 {
                s.push_str(&format!(" (+{delta})"));
            }
            s.push('\n');
        }
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::components::throttle;
    use crate::eval::Evaluator;
    use crate::net::Library;

    const ITEM: ItemType = ItemType(0);
    const TOKEN: ItemType = ItemType(1);

    #[test]
    fn occupancy_and_delivery_read_straight_from_the_algebra() {
        let mut lib = Library::new();
        let id = throttle(&mut lib, ITEM, TOKEN, 3, 4);
        let mut ev = Evaluator::new(&lib);
        let trace = ev.trace_flattened(id, &[Counting::unit_rate()]).unwrap();
        let scene = Scene::new(
            &lib,
            trace,
            vec!["machine".into()],
            vec!["out".into()],
            &[(ITEM, "iron"), (TOKEN, "tok")],
        );
        // Input belt fed 1/tick, machine passes 3/4: backlog is t/4-ish.
        // k(4m) = 3m exactly, so at t=100: supplied 100, consumed 75.
        assert_eq!(scene.occupancy(0, 0, 100), 25);
        // Token loop never leaks: occupancy 0 at the loop's own equilibrium.
        assert_eq!(scene.occupancy(0, 1, 100), 0);
        // Delivered = k(t - 4) = 72 at t = 100.
        assert_eq!(scene.delivered(0, 100), 72);
        // Random access far in time costs nothing and stays exact.
        assert_eq!(scene.occupancy(0, 0, 4_000_000), 1_000_000);
        let frame = scene.frame(100);
        assert!(frame.contains("iron:##########25"), "{frame}");
        assert!(frame.contains("out"), "{frame}");
    }
}
