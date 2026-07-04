//! The playground factory shared by the CLI and GUI binaries.
//!
//! A full iron belt feeds a gear assembler; an overflow gate sends 1/3 of
//! the gears into a demand-driven store (drained at 1/2 by a demand clock,
//! so it keeps up) and spills the rest; the store's tap doubles as its
//! level gauge.

use crate::counting::Counting;
use crate::net::{ItemType, Library, NetBuilder, NetId};
use crate::recipe::Recipe;

pub const IRON: ItemType = ItemType(0);
pub const GEAR: ItemType = ItemType(1);
pub const TOK: ItemType = ItemType(2);
pub const GRANT: ItemType = ItemType(3);
pub const TOK2: ItemType = ItemType(4);
pub const DEMAND: ItemType = ItemType(5);
pub const PULSE: ItemType = ItemType(6);

pub struct Demo {
    pub id: NetId,
    pub inputs: Vec<Counting>,
    pub in_labels: Vec<String>,
    pub node_labels: Vec<String>,
    pub out_labels: Vec<String>,
    pub type_names: Vec<(ItemType, &'static str)>,
}

pub fn build(lib: &mut Library) -> Demo {
    let mut b = NetBuilder::new();
    let iron_in = b.input(IRON);
    let gears = b.recipe(Recipe::new(vec![2], vec![1], 3), &[IRON], &[GEAR]);
    let minter = b.recipe(
        Recipe::new(vec![1], vec![1, 1], 3),
        &[TOK],
        &[TOK, GRANT],
    );
    let gate = b.priority(GEAR, GRANT);
    let dclock = b.recipe(
        Recipe::new(vec![1], vec![1, 1], 2),
        &[TOK2],
        &[TOK2, DEMAND],
    );
    let tap = b.recipe(
        Recipe::new(vec![1], vec![1, 1], 1),
        &[GEAR],
        &[GEAR, PULSE],
    );
    let store_gate = b.priority(GEAR, DEMAND);

    b.connect(iron_in, gears.input(0));
    b.connect(gears.output(0), gate.input(0));
    b.connect(minter.output(0), minter.input(0));
    b.marking(minter.input(0), 1);
    b.connect(minter.output(1), gate.input(1));
    b.connect(gate.output(0), tap.input(0)); // granted gears enter the store
    b.connect(store_gate.output(1), tap.input(0)); // undemanded recirculate
    b.connect(tap.output(0), store_gate.input(0));
    b.connect(dclock.output(0), dclock.input(0));
    b.marking(dclock.input(0), 1);
    b.connect(dclock.output(1), store_gate.input(1));

    let delivered = b.output(GEAR);
    let spill = b.output(GEAR);
    let level = b.output(PULSE);
    b.connect(store_gate.output(0), delivered);
    b.connect(gate.output(1), spill);
    b.connect(tap.output(1), level);

    Demo {
        id: lib.intern(b.build()).expect("demo net is valid"),
        inputs: vec![Counting::unit_rate()],
        in_labels: vec!["iron mine".into()],
        node_labels: vec![
            "gear assembler".into(),
            "grant clock (1/3)".into(),
            "overflow gate".into(),
            "demand clock (1/2)".into(),
            "store tap".into(),
            "store gate".into(),
        ],
        out_labels: vec!["delivered".into(), "spilled".into(), "level pulses".into()],
        type_names: vec![
            (IRON, "iron"),
            (GEAR, "gear"),
            (TOK, "tok"),
            (GRANT, "grant"),
            (TOK2, "tok2"),
            (DEMAND, "demand"),
            (PULSE, "pulse"),
        ],
    }
}
