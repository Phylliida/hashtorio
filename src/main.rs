//! The hashtorio playground: a small factory you can watch, warp, and audit.
//!
//! The whole point on display: the engine never simulates during play.
//! Every frame — including `warp 1000000` — is an O(1) read against exact
//! ultimately-periodic counting maps computed (and cached) once.

use std::io::{BufRead, Write as _};

use hashtorio::counting::Counting;
use hashtorio::eval::Evaluator;
use hashtorio::net::{ItemType, Library, NetBuilder};
use hashtorio::recipe::Recipe;
use hashtorio::render::Scene;

const IRON: ItemType = ItemType(0);
const GEAR: ItemType = ItemType(1);
const TOK: ItemType = ItemType(2);
const GRANT: ItemType = ItemType(3);
const TOK2: ItemType = ItemType(4);
const DEMAND: ItemType = ItemType(5);
const PULSE: ItemType = ItemType(6);

fn main() {
    let mut lib = Library::new();

    // The demo factory: a full iron belt feeds a gear assembler; an
    // overflow gate sends 1/3 of the gears to a demand-driven store (drained
    // at 1/2 by a demand clock, so it keeps up) and spills the rest; the
    // store's tap doubles as its level gauge.
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

    let id = lib.intern(b.build()).expect("demo net is valid");
    let inputs = vec![Counting::unit_rate()];

    let mut ev = Evaluator::new(&lib);
    let summary = ev.summarize(id, &inputs).expect("demo summarizes");
    let audit = ev.audit(id, &inputs).expect("demo audits");
    let trace = ev.trace_flattened(id, &inputs).expect("demo traces");

    let type_names: Vec<(ItemType, &str)> = vec![
        (IRON, "iron"),
        (GEAR, "gear"),
        (TOK, "tok"),
        (GRANT, "grant"),
        (TOK2, "tok"),
        (DEMAND, "demand"),
        (PULSE, "pulse"),
    ];
    let scene = Scene::new(
        &lib,
        trace,
        vec![
            "gear assembler".into(),
            "grant clock (1/3)".into(),
            "overflow gate".into(),
            "demand clock (1/2)".into(),
            "store tap".into(),
            "store gate".into(),
        ],
        vec!["delivered".into(), "spilled".into(), "level pulses".into()],
        &type_names,
    );

    println!("hashtorio playground — a factory as a theorem\n");
    println!("published spec (exact, from the cache entry):");
    for (o, port) in summary.outputs.iter().enumerate() {
        let name = ["delivered", "spilled", "level pulses"][o];
        let first = port
            .first
            .map(|t| format!("first at t={t}"))
            .unwrap_or_else(|| "never".into());
        println!("  {name:<14} rate {}/{} per tick, {first}", port.rate.0, port.rate.1);
    }
    println!("\nconservation audit (exact rationals):");
    for row in &audit.types {
        let name = type_names
            .iter()
            .find(|(ty, _)| *ty == row.ty)
            .map(|(_, n)| *n)
            .unwrap_or("?");
        println!(
            "  {name:<7} in {}/{} minted {}/{} delivered {}/{} accumulating {}/{}",
            row.injected.0, row.injected.1, row.minted.0, row.minted.1,
            row.delivered.0, row.delivered.1, row.accumulating.0, row.accumulating.1,
        );
    }

    println!("\ncommands: <enter> step | run N | warp T | spec | q");
    let stdin = std::io::stdin();
    let mut t: u64 = 0;
    println!("\n{}", scene.frame(t));
    print!("> ");
    std::io::stdout().flush().ok();
    for line in stdin.lock().lines() {
        let line = line.unwrap_or_default();
        let words: Vec<&str> = line.split_whitespace().collect();
        match words.as_slice() {
            [] => {
                t += 1;
                println!("{}", scene.frame(t));
            }
            ["q" | "quit" | "exit"] => break,
            ["run", n] => {
                let n: u64 = n.parse().unwrap_or(10);
                for _ in 0..n {
                    t += 1;
                    print!("\x1b[2J\x1b[H{}", scene.frame(t));
                    std::io::stdout().flush().ok();
                    std::thread::sleep(std::time::Duration::from_millis(60));
                }
                println!();
            }
            ["warp", tt] => {
                // The flex: any tick, instantly, exactly.
                t = tt.parse().unwrap_or(t);
                println!("{}", scene.frame(t));
            }
            ["spec"] => {
                for (o, port) in summary.outputs.iter().enumerate() {
                    let name = ["delivered", "spilled", "level pulses"][o];
                    println!("  {name}: {}/{} per tick", port.rate.0, port.rate.1);
                }
            }
            _ => println!("commands: <enter> step | run N | warp T | spec | q"),
        }
        print!("> ");
        std::io::stdout().flush().ok();
    }
}
