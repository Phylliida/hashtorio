//! The hashtorio playground: a small factory you can watch, warp, and audit.
//!
//! The whole point on display: the engine never simulates during play.
//! Every frame — including `warp 1000000` — is an O(1) read against exact
//! ultimately-periodic counting maps computed (and cached) once.

use std::io::{BufRead, Write as _};

use hashtorio::demo;
use hashtorio::eval::Evaluator;
use hashtorio::net::Library;
use hashtorio::render::Scene;

fn main() {
    let mut lib = Library::new();
    let mut structs = hashtorio::structure::StructLib::new();
    let draft = demo::draft(&mut structs);
    let built = draft.build(&mut lib, &mut structs).expect("demo draft compiles");
    let (id, inputs) = (built.id, built.flows);

    let mut ev = Evaluator::new(&lib);
    let summary = ev.summarize(id, &inputs).expect("demo summarizes");
    let audit = ev.audit(id, &inputs).expect("demo audits");
    let trace = ev.trace_flattened(id, &inputs).expect("demo traces");

    // Names for every interned structure (constructed ones included).
    let owned_names: Vec<(hashtorio::net::ItemType, String)> = (0..structs.len())
        .map(|i| {
            let ty = hashtorio::net::ItemType(i as u32);
            (ty, structs.name(ty))
        })
        .collect();
    let type_names: Vec<(hashtorio::net::ItemType, &str)> =
        owned_names.iter().map(|(t, n)| (*t, n.as_str())).collect();
    // The terminal view renders the FLATTENED net (the demo contains a
    // sealed module), so label nodes by their own shape.
    let tn = |ty: hashtorio::net::ItemType| structs.name(ty);
    let node_labels: Vec<String> = trace
        .net
        .nodes
        .iter()
        .map(|node| match node {
            hashtorio::net::Node::Recipe { recipe, in_types, out_types } => {
                let side = |tys: &[hashtorio::net::ItemType], amts: &[u64]| {
                    tys.iter()
                        .zip(amts)
                        .map(|(t, a)| format!("{a} {}", tn(*t)))
                        .collect::<Vec<_>>()
                        .join("+")
                };
                format!(
                    "{}>{} @{}",
                    side(in_types, &recipe.consume),
                    side(out_types, &recipe.produce),
                    recipe.latency
                )
            }
            hashtorio::net::Node::Priority { item, token } => {
                format!("if {} else... ({})", tn(*token), tn(*item))
            }
            hashtorio::net::Node::Module(_) => "module".into(),
        })
        .collect();
    let out_labels = draft.outputs.iter().map(|o| o.label.clone()).collect();
    let scene = Scene::new(&lib, trace, node_labels, out_labels, &type_names);

    println!("hashtorio playground — a factory as a theorem\n");
    println!("published spec (exact, from the cache entry):");
    for (o, port) in summary.outputs.iter().enumerate() {
        let name = &draft.outputs[o].label;
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
                    let name = &draft.outputs[o].label;
                    println!("  {name}: {}/{} per tick", port.rate.0, port.rate.1);
                }
            }
            _ => println!("commands: <enter> step | run N | warp T | spec | q"),
        }
        print!("> ");
        std::io::stdout().flush().ok();
    }
}
