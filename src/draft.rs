//! The editable blueprint model: what the GUI editor manipulates and what
//! compiles into an interned net.
//!
//! A [`Draft`] is deliberately dumber than a [`crate::net::Net`]: flat lists
//! of inputs, outputs, nodes, wires, and markings, in construction order, so
//! that editor positions map 1:1 onto compiled indices. `build()` validates
//! with *friendly* errors — the compile button talks to a player, and the
//! refusals are the game teaching its own rules ("items can't be copied",
//! "add latency somewhere in that loop").

use crate::counting::Counting;
use crate::net::{ItemType, Library, NetBuilder, NetError, NetId};
use crate::recipe::Recipe;

#[derive(Debug, Clone)]
pub struct DraftInput {
    pub ty: ItemType,
    pub label: String,
    /// Supply rate as items per ticks: `num` items every `den` ticks.
    pub rate: (u64, u64),
}

#[derive(Debug, Clone)]
pub struct DraftOutput {
    pub ty: ItemType,
    pub label: String,
}

#[derive(Debug, Clone)]
pub enum DraftNode {
    Recipe {
        label: String,
        consume: Vec<(ItemType, u64)>,
        produce: Vec<(ItemType, u64)>,
        latency: u64,
    },
    Priority {
        label: String,
        item: ItemType,
        token: ItemType,
    },
}

impl DraftNode {
    pub fn label(&self) -> &str {
        match self {
            DraftNode::Recipe { label, .. } | DraftNode::Priority { label, .. } => label,
        }
    }

    pub fn in_types(&self) -> Vec<ItemType> {
        match self {
            DraftNode::Recipe { consume, .. } => consume.iter().map(|&(t, _)| t).collect(),
            DraftNode::Priority { item, token, .. } => vec![*item, *token],
        }
    }

    pub fn out_types(&self) -> Vec<ItemType> {
        match self {
            DraftNode::Recipe { produce, .. } => produce.iter().map(|&(t, _)| t).collect(),
            DraftNode::Priority { item, .. } => vec![*item, *item],
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DraftFrom {
    Input(usize),
    Node(usize, usize),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DraftTo {
    Node(usize, usize),
    Output(usize),
}

#[derive(Debug, Clone, Default)]
pub struct Draft {
    /// The item palette: (id, display name).
    pub types: Vec<(ItemType, String)>,
    pub inputs: Vec<DraftInput>,
    pub outputs: Vec<DraftOutput>,
    pub nodes: Vec<DraftNode>,
    pub wires: Vec<(DraftFrom, DraftTo)>,
    pub markings: Vec<(DraftTo, u64)>,
}

impl Draft {
    pub fn type_name(&self, ty: ItemType) -> &str {
        self.types
            .iter()
            .find(|(t, _)| *t == ty)
            .map(|(_, n)| n.as_str())
            .unwrap_or("?")
    }

    /// The sink's item type (for edge coloring and type checks).
    pub fn sink_type(&self, to: DraftTo) -> Option<ItemType> {
        match to {
            DraftTo::Node(n, leg) => self.nodes.get(n)?.in_types().get(leg).copied(),
            DraftTo::Output(o) => self.outputs.get(o).map(|out| out.ty),
        }
    }

    fn check(&self) -> Result<(), String> {
        for (i, node) in self.nodes.iter().enumerate() {
            if let DraftNode::Recipe { consume, produce, .. } = node {
                if consume.is_empty() {
                    return Err(format!(
                        "machine {i} ({}) needs at least one input",
                        node.label()
                    ));
                }
                if consume.iter().chain(produce).any(|&(_, amt)| amt == 0) {
                    return Err(format!(
                        "machine {i} ({}) has a zero amount in its recipe",
                        node.label()
                    ));
                }
            }
        }
        let from_ok = |f: &DraftFrom| match *f {
            DraftFrom::Input(i) => i < self.inputs.len(),
            DraftFrom::Node(n, leg) => {
                self.nodes.get(n).is_some_and(|nd| leg < nd.out_types().len())
            }
        };
        let to_ok = |t: &DraftTo| match *t {
            DraftTo::Output(o) => o < self.outputs.len(),
            DraftTo::Node(n, leg) => {
                self.nodes.get(n).is_some_and(|nd| leg < nd.in_types().len())
            }
        };
        for (f, t) in &self.wires {
            if !from_ok(f) || !to_ok(t) {
                return Err("a wire points at something that no longer exists".into());
            }
        }
        for (t, _) in &self.markings {
            if !to_ok(t) {
                return Err("a marking sits on a port that no longer exists".into());
            }
        }
        for input in &self.inputs {
            if input.rate.1 == 0 {
                return Err(format!("source '{}' has a zero-tick period", input.label));
            }
        }
        Ok(())
    }

    /// Compile: build, intern, and return the net plus its input flows.
    pub fn build(&self, lib: &mut Library) -> Result<(NetId, Vec<Counting>), String> {
        self.check()?;
        let mut b = NetBuilder::new();
        let ins: Vec<_> = self.inputs.iter().map(|i| b.input(i.ty)).collect();
        let handles: Vec<_> = self
            .nodes
            .iter()
            .map(|node| match node {
                DraftNode::Recipe { consume, produce, latency, .. } => {
                    let in_tys: Vec<ItemType> = consume.iter().map(|&(t, _)| t).collect();
                    let out_tys: Vec<ItemType> = produce.iter().map(|&(t, _)| t).collect();
                    b.recipe(
                        Recipe::new(
                            consume.iter().map(|&(_, a)| a).collect(),
                            produce.iter().map(|&(_, a)| a).collect(),
                            *latency,
                        ),
                        &in_tys,
                        &out_tys,
                    )
                }
                DraftNode::Priority { item, token, .. } => b.priority(*item, *token),
            })
            .collect();
        let outs: Vec<_> = self.outputs.iter().map(|o| b.output(o.ty)).collect();

        for (from, to) in &self.wires {
            let src = match *from {
                DraftFrom::Input(i) => ins[i],
                DraftFrom::Node(n, leg) => handles[n].output(leg as u32),
            };
            let sink = match *to {
                DraftTo::Node(n, leg) => handles[n].input(leg as u32),
                DraftTo::Output(o) => outs[o],
            };
            b.connect(src, sink);
        }
        for (to, m) in &self.markings {
            let sink = match *to {
                DraftTo::Node(n, leg) => handles[n].input(leg as u32),
                DraftTo::Output(o) => outs[o],
            };
            b.marking(sink, *m);
        }

        let id = lib.intern(b.build()).map_err(|e| friendly_net_error(&e, self))?;
        let flows = self
            .inputs
            .iter()
            .map(|i| {
                if i.rate.0 == 0 {
                    Counting::zero()
                } else {
                    Counting::unit_rate().scale_floor(i.rate.0, i.rate.1)
                }
            })
            .collect();
        Ok((id, flows))
    }
}

fn friendly_net_error(e: &NetError, draft: &Draft) -> String {
    match e {
        NetError::SourceUsedTwice(_) => "an output feeds two different wires — items can't \
            be copied; split the flow with a recipe (e.g. 2·x → 1·x + 1·x)"
            .into(),
        NetError::TypeMismatch { wire, .. } => {
            format!("type mismatch on a wire (sink #{wire}) — the item kinds don't agree")
        }
        NetError::PassThroughCycle => {
            "a loop made only of wires — items would circulate with nothing consuming them"
                .into()
        }
        other => {
            let _ = draft;
            format!("invalid blueprint: {other:?}")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::eval::Evaluator;

    fn iron() -> ItemType {
        ItemType(0)
    }
    fn gear() -> ItemType {
        ItemType(2)
    }

    #[test]
    fn a_minimal_draft_compiles_and_summarizes() {
        let mut d = Draft {
            types: vec![(iron(), "iron".into()), (gear(), "gear".into())],
            ..Default::default()
        };
        d.inputs.push(DraftInput { ty: iron(), label: "mine".into(), rate: (1, 1) });
        d.nodes.push(DraftNode::Recipe {
            label: "gears".into(),
            consume: vec![(iron(), 2)],
            produce: vec![(gear(), 1)],
            latency: 3,
        });
        d.outputs.push(DraftOutput { ty: gear(), label: "out".into() });
        d.wires.push((DraftFrom::Input(0), DraftTo::Node(0, 0)));
        d.wires.push((DraftFrom::Node(0, 0), DraftTo::Output(0)));

        let mut lib = Library::new();
        let (id, flows) = d.build(&mut lib).unwrap();
        let mut ev = Evaluator::new(&lib);
        let s = ev.summarize(id, &flows).unwrap();
        assert_eq!(s.outputs[0].rate, (1, 2));
        assert_eq!(s.outputs[0].first, Some(5));
    }

    #[test]
    fn friendly_errors_teach_the_rules() {
        // Copying: one source, two wires.
        let mut d = Draft {
            types: vec![(iron(), "iron".into())],
            ..Default::default()
        };
        d.inputs.push(DraftInput { ty: iron(), label: "mine".into(), rate: (1, 1) });
        d.outputs.push(DraftOutput { ty: iron(), label: "a".into() });
        d.outputs.push(DraftOutput { ty: iron(), label: "b".into() });
        d.wires.push((DraftFrom::Input(0), DraftTo::Output(0)));
        d.wires.push((DraftFrom::Input(0), DraftTo::Output(1)));
        let mut lib = Library::new();
        let err = d.build(&mut lib).unwrap_err();
        assert!(err.contains("can't be copied"), "{err}");

        // Dangling wire after a (simulated) deletion.
        let mut d2 = Draft {
            types: vec![(iron(), "iron".into())],
            ..Default::default()
        };
        d2.inputs.push(DraftInput { ty: iron(), label: "mine".into(), rate: (1, 1) });
        d2.wires.push((DraftFrom::Input(0), DraftTo::Node(7, 0)));
        let err = d2.build(&mut Library::new()).unwrap_err();
        assert!(err.contains("no longer exists"), "{err}");
    }
}
