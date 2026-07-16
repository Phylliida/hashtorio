//! The playground factory, v3: it builds *structures*, not numbers.
//!
//! Iron and copper cells are welded into a bar, split, one arm rotated,
//! and the two welded again into an L-shaped four-cell piece — which is
//! exactly the **welder's own chassis**: the demo factory manufactures the
//! machine that built it. Finished chassis pass through a sealed demand
//! store (module) on their way out.

use crate::draft::{
    BuildOp, Draft, DraftFrom, DraftInput, DraftNode, DraftOutput, DraftTo,
};
use crate::net::ItemType;
use crate::structure::{chassis, StructLib};

pub const IRON: ItemType = ItemType(0);
pub const COPPER: ItemType = ItemType(1);
pub const GEAR: ItemType = ItemType(2);
pub const PLATE: ItemType = ItemType(3);
pub const TOK: ItemType = ItemType(4);
pub const GRANT: ItemType = ItemType(5);
pub const DEMAND: ItemType = ItemType(6);
pub const PULSE: ItemType = ItemType(7);

/// The fixed *primitive* palette (single cells). Constructed structures
/// get their ids from the [`StructLib`] as play invents them.
pub fn palette() -> Vec<(ItemType, String)> {
    crate::structure::MATERIALS
        .iter()
        .enumerate()
        .map(|(i, n)| (ItemType(i as u32), n.to_string()))
        .collect()
}

/// The first manufacturing goal: the welder's own chassis.
pub fn target(structs: &mut StructLib) -> ItemType {
    chassis(structs, "weld")
}

/// The demand store as a sealed module, generic over the stored item type.
pub fn store_module(item: ItemType) -> Draft {
    let mut d = Draft { types: palette(), ..Default::default() };
    d.inputs.push(DraftInput { ty: item, label: "items in".into(), rate: (1, 1) });
    d.inputs.push(DraftInput { ty: DEMAND, label: "demand".into(), rate: (1, 1) });
    d.nodes.push(DraftNode::Recipe {
        label: "tap".into(),
        consume: vec![(item, 1)],
        produce: vec![(item, 1), (PULSE, 1)],
        latency: 1,
    });
    d.nodes.push(DraftNode::Priority {
        label: "gate".into(),
        item,
        token: DEMAND,
    });
    d.outputs.push(DraftOutput { ty: item, label: "out".into() });
    d.outputs.push(DraftOutput { ty: PULSE, label: "level".into() });
    use DraftFrom as F;
    use DraftTo as T;
    d.wires = vec![
        (F::Input(0), T::Node(0, 0)),   // arriving items join the pool
        (F::Node(1, 1), T::Node(0, 0)), // undemanded items recirculate
        (F::Node(0, 0), T::Node(1, 0)), // tap -> gate
        (F::Input(1), T::Node(1, 1)),   // demand tokens
        (F::Node(1, 0), T::Output(0)),  // granted items leave
        (F::Node(0, 1), T::Output(1)),  // census pulses
    ];
    // Interior space of the pocket dimension.
    d.input_pos = vec![(2, 2), (2, 8)];
    d.node_pos = vec![(6, 2), (12, 2)];
    d.output_pos = vec![(18, 2), (18, 8)];
    d
}

pub fn draft(structs: &mut StructLib) -> Draft {
    let goal = target(structs);
    let mut d = Draft { types: palette(), ..Default::default() };
    d.inputs.push(DraftInput { ty: IRON, label: "iron mine".into(), rate: (1, 1) });
    d.inputs.push(DraftInput { ty: COPPER, label: "copper mine".into(), rate: (1, 1) });
    d.nodes.push(DraftNode::Builder {
        label: "welder A".into(),
        op: BuildOp::Weld { dx: 1, dy: 0 },
        latency: 2,
    });
    d.nodes.push(DraftNode::Builder {
        label: "splitter".into(),
        op: BuildOp::Split,
        latency: 1,
    });
    d.nodes.push(DraftNode::Builder {
        label: "rotator".into(),
        op: BuildOp::Rot,
        latency: 1,
    });
    d.nodes.push(DraftNode::Builder {
        label: "welder B".into(),
        op: BuildOp::Weld { dx: 0, dy: 2 },
        latency: 2,
    });
    // In factory-space the clock's feedback loop must round its own
    // chassis — 5 cells of placed belt, 2 ticks. That alone is the whole
    // period: recipe latency 0 gives the intended 1/2 demand beat. (Under
    // the old Manhattan rule this loop pretended to be 1 tick; routed
    // belts repriced it, exactly the "space is semantic" lesson of M10.)
    d.nodes.push(DraftNode::Recipe {
        label: "demand clock (1/2)".into(),
        consume: vec![(TOK, 1)],
        produce: vec![(TOK, 1), (DEMAND, 1)],
        latency: 0,
    });
    d.nodes.push(DraftNode::Module {
        label: "chassis store".into(),
        draft: Box::new(store_module(goal)),
    });
    d.outputs.push(DraftOutput { ty: goal, label: "welders built".into() });
    d.outputs.push(DraftOutput { ty: PULSE, label: "store level".into() });

    use DraftFrom as F;
    use DraftTo as T;
    d.wires = vec![
        (F::Input(0), T::Node(0, 0)),   // iron -> welder A
        (F::Input(1), T::Node(0, 1)),   // copper -> welder A
        (F::Node(0, 0), T::Node(1, 0)), // bar -> splitter
        (F::Node(1, 0), T::Node(2, 0)), // one arm -> rotator
        (F::Node(2, 0), T::Node(3, 0)), // vertical bar -> welder B
        (F::Node(1, 1), T::Node(3, 1)), // straight arm -> welder B
        (F::Node(4, 0), T::Node(4, 0)), // demand clock loop
        (F::Node(3, 0), T::Node(5, 0)), // chassis -> store
        (F::Node(4, 1), T::Node(5, 1)), // demand -> store
        (F::Node(5, 0), T::Output(0)),  // welders out
        (F::Node(5, 1), T::Output(1)),  // level pulses
    ];
    d.markings = vec![(T::Node(4, 0), 1)];
    // Factory-space: the layout is semantic — distance is latency.
    d.input_pos = vec![(2, 6), (2, 10)];
    d.node_pos = vec![
        (6, 6),   // welder A
        (12, 6),  // splitter
        (18, 2),  // rotator
        (24, 6),  // welder B
        (18, 14), // demand clock
        (31, 6),  // chassis store
    ];
    d.output_pos = vec![(38, 6), (38, 12)];
    d
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::eval::Evaluator;
    use crate::net::Library;

    #[test]
    fn the_factory_manufactures_its_own_welder() {
        let mut lib = Library::new();
        let mut structs = StructLib::new();
        let d = draft(&mut structs);
        let built = d.build(&mut lib, &mut structs).unwrap();
        let (id, flows, node_types) = (built.id, built.flows, built.node_types);

        // Inference found the chassis: welder B's output IS the target.
        let goal = target(&mut structs);
        assert_eq!(node_types[3].1, vec![goal], "welder B builds the welder chassis");
        assert_eq!(structs.cells(goal).len(), 4);
        assert_eq!(structs.name(goal), "weld chassis");

        // Rates: mines 1/1 -> bar 1/1 -> split halves -> chassis 1/2,
        // demand 1/2: delivered at 1/2.
        let mut ev = Evaluator::new(&lib);
        let s = ev.summarize(id, &flows).unwrap();
        assert_eq!(s.outputs[0].rate, (1, 2)); // welders built
        assert_eq!(s.outputs[0].ty, goal);
    }

    #[test]
    fn identical_modules_intern_once() {
        let mut lib = Library::new();
        let mut structs = StructLib::new();
        let mut d = draft(&mut structs);
        let goal = target(&mut structs);
        d.nodes.push(DraftNode::Module {
            label: "spare store".into(),
            draft: Box::new(store_module(goal)),
        });
        d.node_pos.push((31, 16)); // parked off to the side
        d.build(&mut lib, &mut structs).unwrap();
        assert_eq!(lib.len(), 2, "identical sub-drafts dedup to one NetId");
    }
}
