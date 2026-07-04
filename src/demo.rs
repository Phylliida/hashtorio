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
    d.nodes.push(DraftNode::Recipe {
        label: "store tap".into(),
        consume: vec![(GEAR, 1)],
        produce: vec![(GEAR, 1), (PULSE, 1)],
        latency: 1,
    });
    d.nodes.push(DraftNode::Priority {
        label: "store gate".into(),
        item: GEAR,
        token: DEMAND,
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
        (F::Node(2, 0), T::Node(4, 0)), // granted gears -> store pool (tap)
        (F::Node(5, 1), T::Node(4, 0)), // undemanded gears recirculate
        (F::Node(4, 0), T::Node(5, 0)), // tap -> store gate
        (F::Node(3, 0), T::Node(3, 0)), // demand clock loop
        (F::Node(3, 1), T::Node(5, 1)), // demand -> store gate tokens
        (F::Node(5, 0), T::Output(0)),  // delivered
        (F::Node(2, 1), T::Output(1)),  // spilled
        (F::Node(4, 1), T::Output(2)),  // level pulses
    ];
    d.markings = vec![(T::Node(1, 0), 1), (T::Node(3, 0), 1)];
    d
}
