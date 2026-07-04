//! The playground factory, expressed as a [`Draft`] — the same model the
//! GUI editor produces, so "load the demo into the editor" is trivial.
//!
//! A full iron belt feeds a gear assembler; an overflow gate sends 1/3 of
//! the gears into a demand-driven store (drained at 1/2 by a demand clock,
//! so it keeps up) and spills the rest; the store's tap doubles as its
//! level gauge.

use crate::draft::{Draft, DraftFrom, DraftInput, DraftNode, DraftOutput, DraftTo};
use crate::net::ItemType;

pub const IRON: ItemType = ItemType(0);
pub const COPPER: ItemType = ItemType(1);
pub const GEAR: ItemType = ItemType(2);
pub const PLATE: ItemType = ItemType(3);
pub const TOK: ItemType = ItemType(4);
pub const GRANT: ItemType = ItemType(5);
pub const DEMAND: ItemType = ItemType(6);
pub const PULSE: ItemType = ItemType(7);

/// The fixed item palette shared by demo, editor, and GUI.
pub fn palette() -> Vec<(ItemType, String)> {
    [
        (IRON, "iron"),
        (COPPER, "copper"),
        (GEAR, "gear"),
        (PLATE, "plate"),
        (TOK, "tok"),
        (GRANT, "grant"),
        (DEMAND, "demand"),
        (PULSE, "pulse"),
    ]
    .into_iter()
    .map(|(t, n)| (t, n.to_string()))
    .collect()
}

/// The demand store as a sealed module: its recirculation loop is fully
/// contained, so from outside it is just (gears in, demand in) ->
/// (gears out, level pulses). The abstraction boundary made visible.
pub fn store_module() -> Draft {
    let mut d = Draft { types: palette(), ..Default::default() };
    d.inputs.push(DraftInput { ty: GEAR, label: "gears in".into(), rate: (1, 1) });
    d.inputs.push(DraftInput { ty: DEMAND, label: "demand".into(), rate: (1, 1) });
    d.nodes.push(DraftNode::Recipe {
        label: "tap".into(),
        consume: vec![(GEAR, 1)],
        produce: vec![(GEAR, 1), (PULSE, 1)],
        latency: 1,
    });
    d.nodes.push(DraftNode::Priority {
        label: "gate".into(),
        item: GEAR,
        token: DEMAND,
    });
    d.outputs.push(DraftOutput { ty: GEAR, label: "out".into() });
    d.outputs.push(DraftOutput { ty: PULSE, label: "level".into() });
    use DraftFrom as F;
    use DraftTo as T;
    d.wires = vec![
        (F::Input(0), T::Node(0, 0)),   // arriving gears join the pool
        (F::Node(1, 1), T::Node(0, 0)), // undemanded gears recirculate
        (F::Node(0, 0), T::Node(1, 0)), // tap -> gate
        (F::Input(1), T::Node(1, 1)),   // demand tokens
        (F::Node(1, 0), T::Output(0)),  // granted gears leave
        (F::Node(0, 1), T::Output(1)),  // census pulses
    ];
    d
}

pub fn draft() -> Draft {
    let mut d = Draft { types: palette(), ..Default::default() };
    d.inputs.push(DraftInput { ty: IRON, label: "iron mine".into(), rate: (1, 1) });
    d.nodes.push(DraftNode::Recipe {
        label: "gear assembler".into(),
        consume: vec![(IRON, 2)],
        produce: vec![(GEAR, 1)],
        latency: 3,
    });
    d.nodes.push(DraftNode::Recipe {
        label: "grant clock (1/3)".into(),
        consume: vec![(TOK, 1)],
        produce: vec![(TOK, 1), (GRANT, 1)],
        latency: 3,
    });
    d.nodes.push(DraftNode::Priority {
        label: "overflow gate".into(),
        item: GEAR,
        token: GRANT,
    });
    d.nodes.push(DraftNode::Recipe {
        label: "demand clock (1/2)".into(),
        consume: vec![(TOK, 1)],
        produce: vec![(TOK, 1), (DEMAND, 1)],
        latency: 2,
    });
    d.nodes.push(DraftNode::Module {
        label: "demand store".into(),
        draft: Box::new(store_module()),
    });
    d.outputs.push(DraftOutput { ty: GEAR, label: "delivered".into() });
    d.outputs.push(DraftOutput { ty: GEAR, label: "spilled".into() });
    d.outputs.push(DraftOutput { ty: PULSE, label: "level pulses".into() });

    use DraftFrom as F;
    use DraftTo as T;
    d.wires = vec![
        (F::Input(0), T::Node(0, 0)),   // iron -> assembler
        (F::Node(0, 0), T::Node(2, 0)), // gears -> overflow gate
        (F::Node(1, 0), T::Node(1, 0)), // grant clock loop
        (F::Node(1, 1), T::Node(2, 1)), // grants -> gate tokens
        (F::Node(2, 0), T::Node(4, 0)), // granted gears -> store module
        (F::Node(3, 0), T::Node(3, 0)), // demand clock loop
        (F::Node(3, 1), T::Node(4, 1)), // demand -> store module
        (F::Node(4, 0), T::Output(0)),  // delivered
        (F::Node(2, 1), T::Output(1)),  // spilled
        (F::Node(4, 1), T::Output(2)),  // level pulses
    ];
    d.markings = vec![(T::Node(1, 0), 1), (T::Node(3, 0), 1)];
    d
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::eval::Evaluator;
    use crate::net::Library;

    #[test]
    fn sealed_demo_matches_the_flat_rates() {
        // The store is now a module; sealing must not change semantics.
        let mut lib = Library::new();
        let (id, flows) = draft().build(&mut lib).unwrap();
        let mut ev = Evaluator::new(&lib);
        let s = ev.summarize(id, &flows).unwrap();
        assert_eq!(s.outputs[0].rate, (1, 3)); // delivered
        assert_eq!(s.outputs[1].rate, (1, 6)); // spilled
        assert_eq!(s.outputs[2].rate, (1, 3)); // level pulses
        // Parent-level detail treats the module as one opaque node.
        let d = ev.evaluate_detailed(id, &flows).unwrap();
        assert_eq!(d.node_outs.len(), 5);
        assert!(d.firings[4].is_none(), "module firings are interior");
        assert_eq!(d.node_outs[4][0].rate(), (1, 3));
    }

    #[test]
    fn identical_modules_intern_once() {
        let mut d = draft();
        // A second, identical store module (unwired: just present).
        d.nodes.push(DraftNode::Module {
            label: "spare store".into(),
            draft: Box::new(store_module()),
        });
        let mut lib = Library::new();
        d.build(&mut lib).unwrap();
        // Library holds exactly: the store module net + the parent net.
        assert_eq!(lib.len(), 2, "identical sub-drafts dedup to one NetId");
    }
}
