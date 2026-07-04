//! The hashtorio GUI server: editing, sealing, and viewing factories.
//!
//! GET  /            the app
//! GET  /api/scene   current factory: topology (modules opaque) + spec + audit
//! GET  /api/frames  batched per-tick data (O(1) reads of counting maps)
//! POST /api/compile a draft blueprint (modules nested by value); on success
//!                   it becomes the current factory, on failure the error is
//!                   a friendly refusal — the engine teaching its own rules.
//!
//! The view is strictly parent-level: a sealed module renders as one node
//! with port flows. Its interior is *not hidden by the renderer* — it is
//! absent from the data, because the evaluator answered with the module's
//! memoized summary. The abstraction boundary is real, not cosmetic.

mod json;

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};

use hashtorio::counting::Counting;
use hashtorio::demo;
use hashtorio::draft::{BuildOp, Draft, DraftFrom, DraftInput, DraftNode, DraftOutput, DraftTo};
use hashtorio::eval::{EvalError, Evaluator};
use hashtorio::net::{ItemType, Library};
use hashtorio::structure::{chassis, StructLib, ANY};
use hashtorio::report::{Audit, Summary};
use json::Json;

const INDEX_HTML: &str = include_str!("../../../gui/index.html");
const MAX_BATCH: u64 = 128;
/// Compiles should feel instant; refusals past this horizon are honest.
const GUI_HORIZON: u64 = 4096;

struct Compiled {
    draft: Draft,
    input_flows: Vec<Counting>,
    /// Parent-level output-leg countings per node (modules opaque).
    node_outs: Vec<Vec<Counting>>,
    /// Firing maps; `None` for modules (interior detail stays sealed).
    firings: Vec<Option<Counting>>,
    outputs: Vec<Counting>,
    /// Per node, per input leg: the wire's supply counting map.
    supplies: Vec<Vec<Counting>>,
    /// Per node, per input leg: consumption map; `None` for module legs
    /// (queueing happens inside the seal).
    consumed: Vec<Vec<Option<Counting>>>,
    /// Concrete (in, out) leg types per node, builders resolved.
    node_types: Vec<(Vec<ItemType>, Vec<ItemType>)>,
    /// Geometric latency per wire; arrivals = departures shifted by it.
    wire_lats: Vec<u64>,
    /// Per wire: the arrival counting map at the sink end.
    arrivals: Vec<Counting>,
    summary: Summary,
    audit: Audit,
}

struct App {
    lib: Library,
    structs: StructLib,
    /// Chassis structure per machine kind: machine-types are structures.
    chassis: Vec<(&'static str, ItemType)>,
    /// The manufacturing goal: the welder's own chassis.
    target: ItemType,
    current: Compiled,
}

const KINDS: [&str; 7] = ["weld", "rot", "split", "belt", "recipe", "priority", "module"];

impl App {
    fn new() -> App {
        let mut lib = Library::new();
        let mut structs = StructLib::new();
        let chassis_map: Vec<(&'static str, ItemType)> =
            KINDS.iter().map(|k| (*k, chassis(&mut structs, k))).collect();
        let target = chassis_map[0].1; // the welder chassis
        let draft = hashtorio::demo::draft(&mut structs);
        let current = compile(&mut lib, &mut structs, draft).expect("demo compiles");
        App { lib, structs, chassis: chassis_map, target, current }
    }

    fn kind_chassis(&self, kind: &str) -> ItemType {
        self.chassis
            .iter()
            .find(|(k, _)| *k == kind)
            .map(|(_, t)| *t)
            .expect("known kind")
    }
}

fn friendly_eval_error(e: &EvalError) -> String {
    match e {
        EvalError::ZeroLatencyCycle => {
            "a zero-latency feedback loop (Zeno: infinite firings in one tick) — \
             add latency somewhere in the cycle"
                .into()
        }
        EvalError::RateExplosion => {
            "no steady state: this design grows without bound (a breeder loop?) — \
             the viewer needs a summarizable factory"
                .into()
        }
        EvalError::NoPeriodicSteadyState { horizon } => format!(
            "no periodic steady state found within {horizon} ticks — \
             the design may have an enormous period"
        ),
        EvalError::CycleThroughModule => {
            "a feedback loop crosses a module boundary — seal the whole loop \
             inside the module, or keep it outside"
                .into()
        }
        other => format!("evaluation failed: {other:?}"),
    }
}

fn compile(
    lib: &mut Library,
    structs: &mut StructLib,
    draft: Draft,
) -> Result<Compiled, String> {
    let built = draft.build(lib, structs)?;
    let (id, input_flows, node_types, wire_lats) =
        (built.id, built.flows, built.node_types, built.wire_lats);
    let mut ev = Evaluator::new(lib);
    ev.horizon = GUI_HORIZON;
    let detail = ev
        .evaluate_detailed(id, &input_flows)
        .map_err(|e| friendly_eval_error(&e))?;
    let summary = ev
        .summarize(id, &input_flows)
        .map_err(|e| friendly_eval_error(&e))?;
    let audit = ev
        .audit(id, &input_flows)
        .map_err(|e| friendly_eval_error(&e))?;

    let marking_of = |to: DraftTo| -> u64 {
        draft
            .markings
            .iter()
            .filter(|(t, _)| *t == to)
            .map(|(_, m)| *m)
            .sum()
    };
    let source_counting = |from: &DraftFrom| -> &Counting {
        match from {
            DraftFrom::Input(i) => &input_flows[*i],
            DraftFrom::Node(n, l) => &detail.node_outs[*n][*l],
        }
    };
    // Arrivals at the sink end of each wire: departures shifted by the
    // wire's geometric latency (belts are identity recipes, exactly).
    let arrivals: Vec<Counting> = draft
        .wires
        .iter()
        .zip(&wire_lats)
        .map(|((from, _), &lat)| source_counting(from).shift(lat))
        .collect();
    let mut supplies = Vec::with_capacity(draft.nodes.len());
    let mut consumed = Vec::with_capacity(draft.nodes.len());
    for (n, node) in draft.nodes.iter().enumerate() {
        let legs = node_types[n].0.len();
        let sup: Vec<Counting> = (0..legs)
            .map(|l| {
                let mut acc = Counting::constant(marking_of(DraftTo::Node(n, l)));
                for (w, (_, to)) in draft.wires.iter().enumerate() {
                    if *to == DraftTo::Node(n, l) {
                        acc = acc.add(&arrivals[w]);
                    }
                }
                acc
            })
            .collect();
        let cons: Vec<Option<Counting>> = match node {
            DraftNode::Recipe { consume, .. } => consume
                .iter()
                .map(|&(_, c)| {
                    Some(
                        detail.firings[n]
                            .as_ref()
                            .expect("recipes have firings")
                            .scale_floor(c, 1),
                    )
                })
                .collect(),
            DraftNode::Priority { .. } => vec![
                Some(detail.node_outs[n][0].add(&detail.node_outs[n][1])),
                Some(detail.node_outs[n][0].clone()),
            ],
            DraftNode::Module { .. } => vec![None; legs],
            DraftNode::Builder { op, .. } => {
                let k = detail.firings[n].as_ref().expect("builders compile to recipes");
                let per = if matches!(op, BuildOp::Split) { 2 } else { 1 };
                (0..legs).map(|_| Some(k.scale_floor(per, 1))).collect()
            }
        };
        supplies.push(sup);
        consumed.push(cons);
    }

    Ok(Compiled {
        input_flows,
        node_outs: detail.node_outs,
        firings: detail.firings,
        outputs: detail.outputs,
        supplies,
        consumed,
        node_types,
        wire_lats,
        arrivals,
        summary,
        audit,
        draft,
    })
}

// ---------------------------------------------------------------------------
// JSON out
// ---------------------------------------------------------------------------

fn esc(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

fn rate_json(r: (u64, u64)) -> String {
    format!("[{},{}]", r.0, r.1)
}

fn from_json(f: &DraftFrom) -> String {
    match f {
        DraftFrom::Input(i) => format!("[\"in\",{i}]"),
        DraftFrom::Node(n, l) => format!("[\"node\",{n},{l}]"),
    }
}

fn to_json(t: &DraftTo) -> String {
    match t {
        DraftTo::Node(n, l) => format!("[\"node\",{n},{l}]"),
        DraftTo::Output(o) => format!("[\"out\",{o}]"),
    }
}

fn kind_of(node: &DraftNode) -> &'static str {
    match node {
        DraftNode::Recipe { .. } => "recipe",
        DraftNode::Priority { .. } => "priority",
        DraftNode::Module { .. } => "module",
        DraftNode::Builder { op: BuildOp::Weld { .. }, .. } => "weld",
        DraftNode::Builder { op: BuildOp::Rot, .. } => "rot",
        DraftNode::Builder { op: BuildOp::Split, .. } => "split",
        DraftNode::Builder { op: BuildOp::Belt, .. } => "belt",
    }
}

fn node_json(
    d: &Draft,
    n: usize,
    node: &DraftNode,
    resolved: Option<&(Vec<ItemType>, Vec<ItemType>)>,
) -> String {
    let marking_of = |to: DraftTo| -> u64 {
        d.markings.iter().filter(|(t, _)| *t == to).map(|(_, m)| *m).sum()
    };
    let (in_tys, out_tys) = match resolved {
        Some((i, o)) => (i.clone(), o.clone()),
        None => (node.in_types(), node.out_types()),
    };
    let legs: Vec<String> = in_tys.iter().map(|t| t.0.to_string()).collect();
    let outs: Vec<String> = out_tys.iter().map(|t| t.0.to_string()).collect();
    let markings: Vec<String> = (0..in_tys.len())
        .map(|l| marking_of(DraftTo::Node(n, l)).to_string())
        .collect();
    let core = match node {
        DraftNode::Recipe { consume, produce, latency, .. } => {
            let c: Vec<String> =
                consume.iter().map(|(t, a)| format!("[{},{a}]", t.0)).collect();
            let p: Vec<String> =
                produce.iter().map(|(t, a)| format!("[{},{a}]", t.0)).collect();
            format!(
                "\"kind\":\"recipe\",\"consume\":[{}],\"produce\":[{}],\"latency\":{latency}",
                c.join(","),
                p.join(",")
            )
        }
        DraftNode::Priority { item, token, .. } => format!(
            "\"kind\":\"priority\",\"item\":{},\"token\":{}",
            item.0, token.0
        ),
        DraftNode::Module { draft, .. } => {
            format!("\"kind\":\"module\",\"draft\":{}", draft_json(draft))
        }
        DraftNode::Builder { op, latency, .. } => match op {
            BuildOp::Weld { dx, dy } => format!(
                "\"kind\":\"weld\",\"dx\":{dx},\"dy\":{dy},\"latency\":{latency}"
            ),
            BuildOp::Rot => format!("\"kind\":\"rot\",\"latency\":{latency}"),
            BuildOp::Split => format!("\"kind\":\"split\",\"latency\":{latency}"),
            BuildOp::Belt => format!("\"kind\":\"belt\",\"latency\":{latency}"),
        },
    };
    format!(
        "{{{core},\"label\":\"{}\",\"legs\":[{}],\"outs\":[{}],\"markings\":[{}]}}",
        esc(node.label()),
        legs.join(","),
        outs.join(","),
        markings.join(",")
    )
}

fn cells_json(structs: &StructLib, ty: ItemType) -> String {
    let cells: Vec<String> = structs
        .cells(ty)
        .iter()
        .map(|(x, y, m)| format!("[{x},{y},{m}]"))
        .collect();
    format!("[{}]", cells.join(","))
}

/// A draft in the editor's own format (recursive; round-trips through
/// POST /api/compile).
fn draft_json(d: &Draft) -> String {
    let inputs: Vec<String> = d
        .inputs
        .iter()
        .map(|i| {
            format!(
                "{{\"ty\":{},\"label\":\"{}\",\"rate\":{}}}",
                i.ty.0,
                esc(&i.label),
                rate_json(i.rate)
            )
        })
        .collect();
    let outputs: Vec<String> = d
        .outputs
        .iter()
        .map(|o| format!("{{\"ty\":{},\"label\":\"{}\"}}", o.ty.0, esc(&o.label)))
        .collect();
    let nodes: Vec<String> = d
        .nodes
        .iter()
        .enumerate()
        .map(|(n, node)| node_json(d, n, node, None))
        .collect();
    let wires: Vec<String> = d
        .wires
        .iter()
        .map(|(f, t)| format!("{{\"from\":{},\"to\":{}}}", from_json(f), to_json(t)))
        .collect();
    let markings: Vec<String> = d
        .markings
        .iter()
        .map(|(t, m)| format!("{{\"to\":{},\"n\":{m}}}", to_json(t)))
        .collect();
    let pos_list = |ps: &[(i32, i32)]| -> String {
        let items: Vec<String> = ps.iter().map(|(x, y)| format!("[{x},{y}]")).collect();
        format!("[{}]", items.join(","))
    };
    format!(
        "{{\"inputs\":[{}],\"outputs\":[{}],\"nodes\":[{}],\"wires\":[{}],\"markings\":[{}],\
         \"pos\":{{\"inputs\":{},\"nodes\":{},\"outputs\":{}}}}}",
        inputs.join(","),
        outputs.join(","),
        nodes.join(","),
        wires.join(","),
        markings.join(","),
        pos_list(&d.input_pos),
        pos_list(&d.node_pos),
        pos_list(&d.output_pos)
    )
}

fn scene_json(app: &App) -> String {
    let c = &app.current;
    let structs = &app.structs;
    let d = &c.draft;
    let marking_of = |to: DraftTo| -> u64 {
        d.markings.iter().filter(|(t, _)| *t == to).map(|(_, m)| *m).sum()
    };
    // Resolved sink type of a wire (builders concrete post-inference).
    let edge_ty = |to: &DraftTo| -> ItemType {
        match to {
            DraftTo::Node(n, l) => c.node_types[*n].0[*l],
            DraftTo::Output(o) => d.outputs[*o].ty,
        }
    };
    // Every type id the frontend will need to draw, with its cells.
    // The eight primitives are always present (editor prompts name them).
    let mut used: Vec<u32> = (0..8).collect();
    let mut note = |t: ItemType| {
        if t != ANY && structs.contains(t) && !used.contains(&t.0) {
            used.push(t.0);
        }
    };
    for i in &d.inputs {
        note(i.ty);
    }
    for o in &d.outputs {
        note(o.ty);
    }
    for (ins, outs) in &c.node_types {
        for t in ins.iter().chain(outs) {
            note(*t);
        }
    }
    note(app.target);
    used.sort_unstable();
    let types: Vec<String> = used
        .iter()
        .map(|&id| {
            let ty = ItemType(id);
            format!(
                "{{\"id\":{id},\"name\":\"{}\",\"cells\":{}}}",
                esc(&structs.name(ty)),
                cells_json(structs, ty)
            )
        })
        .collect();
    let inputs: Vec<String> = d
        .inputs
        .iter()
        .map(|i| {
            format!(
                "{{\"ty\":{},\"label\":\"{}\",\"rate\":{}}}",
                i.ty.0,
                esc(&i.label),
                rate_json(i.rate)
            )
        })
        .collect();
    let outputs: Vec<String> = d
        .outputs
        .iter()
        .enumerate()
        .map(|(o, out)| {
            format!(
                "{{\"ty\":{},\"label\":\"{}\",\"marking\":{}}}",
                out.ty.0,
                esc(&out.label),
                marking_of(DraftTo::Output(o))
            )
        })
        .collect();
    let nodes: Vec<String> = d
        .nodes
        .iter()
        .enumerate()
        .map(|(n, node)| {
            let base = node_json(d, n, node, Some(&c.node_types[n]));
            let ch = app.kind_chassis(kind_of(node));
            format!(
                "{{{},\"chassis\":{}}}",
                &base[1..base.len() - 1],
                cells_json(structs, ch)
            )
        })
        .collect();
    let edges: Vec<String> = d
        .wires
        .iter()
        .map(|(from, to)| {
            format!(
                "{{\"from\":{},\"to\":{},\"ty\":{}}}",
                from_json(from),
                to_json(to),
                edge_ty(to).0
            )
        })
        .collect();
    let spec: Vec<String> = c
        .summary
        .outputs
        .iter()
        .enumerate()
        .map(|(o, p)| {
            format!(
                "{{\"name\":\"{}\",\"rate\":{},\"first\":{}}}",
                esc(&d.outputs[o].label),
                rate_json(p.rate),
                p.first.map(|t| t.to_string()).unwrap_or("null".into())
            )
        })
        .collect();
    let audit: Vec<String> = c
        .audit
        .types
        .iter()
        .map(|r| {
            format!(
                "{{\"ty\":\"{}\",\"injected\":{},\"minted\":{},\"consumed\":{},\
                 \"delivered\":{},\"discarded\":{},\"accumulating\":{}}}",
                esc(&structs.name(r.ty)),
                rate_json(r.injected),
                rate_json(r.minted),
                rate_json(r.consumed),
                rate_json(r.delivered),
                rate_json(r.discarded),
                rate_json(r.accumulating),
            )
        })
        .collect();
    // The manufacturing goal: met when some output emits the target shape.
    let goal = {
        let met = d
            .outputs
            .iter()
            .enumerate()
            .find(|(_, o)| o.ty == app.target)
            .map(|(i, _)| c.summary.outputs[i].rate)
            .filter(|r| r.0 > 0);
        format!(
            "{{\"name\":\"{}\",\"cells\":{},\"met\":{},\"rate\":{}}}",
            esc(&structs.name(app.target)),
            cells_json(structs, app.target),
            met.is_some(),
            met.map(rate_json).unwrap_or("null".into())
        )
    };
    let pos_list = |ps: &[(i32, i32)]| -> String {
        let items: Vec<String> = ps.iter().map(|(x, y)| format!("[{x},{y}]")).collect();
        format!("[{}]", items.join(","))
    };
    let lats: Vec<String> = c.wire_lats.iter().map(|l| l.to_string()).collect();
    format!(
        "{{\"types\":[{}],\"inputs\":[{}],\"outputs\":[{}],\"nodes\":[{}],\
         \"edges\":[{}],\"lats\":[{}],\"pos\":{{\"inputs\":{},\"nodes\":{},\"outputs\":{}}},\
         \"spec\":[{}],\"audit\":[{}],\"goal\":{goal}}}",
        types.join(","),
        inputs.join(","),
        outputs.join(","),
        nodes.join(","),
        edges.join(","),
        lats.join(","),
        pos_list(&d.input_pos),
        pos_list(&d.node_pos),
        pos_list(&d.output_pos),
        spec.join(","),
        audit.join(",")
    )
}

fn frame_json(c: &Compiled, t: u64) -> String {
    let delta = |cnt: &Counting| cnt.eval(t) - if t == 0 { 0 } else { cnt.eval(t - 1) };
    let fired: Vec<String> = (0..c.draft.nodes.len())
        .map(|n| match &c.firings[n] {
            Some(k) => delta(k).to_string(),
            // A module's "activity" is motion on its output ports.
            None => c.node_outs[n].iter().map(&delta).sum::<u64>().to_string(),
        })
        .collect();
    let occ: Vec<String> = (0..c.draft.nodes.len())
        .map(|n| {
            let legs: Vec<String> = c.consumed[n]
                .iter()
                .enumerate()
                .map(|(l, cons)| match cons {
                    Some(cm) => (c.supplies[n][l].eval(t) - cm.eval(t)).to_string(),
                    None => "null".into(), // sealed: queueing is interior
                })
                .collect();
            format!("[{}]", legs.join(","))
        })
        .collect();
    // Arrivals animate at the sink; transit = departed minus arrived,
    // the items physically on the wire right now.
    let flow: Vec<String> = c.arrivals.iter().map(|a| delta(a).to_string()).collect();
    let transit: Vec<String> = c
        .draft
        .wires
        .iter()
        .zip(&c.arrivals)
        .map(|((from, _), arr)| {
            let dep = match from {
                DraftFrom::Input(i) => &c.input_flows[*i],
                DraftFrom::Node(n, l) => &c.node_outs[*n][*l],
            };
            (dep.eval(t) - arr.eval(t)).to_string()
        })
        .collect();
    let outs: Vec<String> = c
        .outputs
        .iter()
        .map(|cnt| format!("[{},{}]", cnt.eval(t), delta(cnt)))
        .collect();
    format!(
        "{{\"fired\":[{}],\"occ\":[{}],\"flow\":[{}],\"transit\":[{}],\"outs\":[{}]}}",
        fired.join(","),
        occ.join(","),
        flow.join(","),
        transit.join(","),
        outs.join(",")
    )
}

fn frames_json(c: &Compiled, from: u64, n: u64) -> String {
    let n = n.clamp(1, MAX_BATCH);
    let frames: Vec<String> = (from..from + n).map(|t| frame_json(c, t)).collect();
    format!("{{\"from\":{from},\"frames\":[{}]}}", frames.join(","))
}

// ---------------------------------------------------------------------------
// JSON in: the draft payload (recursive for modules)
// ---------------------------------------------------------------------------

fn parse_endpoint_from(v: &Json) -> Result<DraftFrom, String> {
    let a = v.arr().ok_or("bad wire endpoint")?;
    match (a.first().and_then(|k| k.str()), a.get(1).and_then(|x| x.usize())) {
        (Some("in"), Some(i)) => Ok(DraftFrom::Input(i)),
        (Some("node"), Some(n)) => {
            let leg = a.get(2).and_then(|x| x.usize()).ok_or("bad wire endpoint")?;
            Ok(DraftFrom::Node(n, leg))
        }
        _ => Err("bad wire endpoint".into()),
    }
}

fn parse_endpoint_to(v: &Json) -> Result<DraftTo, String> {
    let a = v.arr().ok_or("bad wire endpoint")?;
    match (a.first().and_then(|k| k.str()), a.get(1).and_then(|x| x.usize())) {
        (Some("out"), Some(o)) => Ok(DraftTo::Output(o)),
        (Some("node"), Some(n)) => {
            let leg = a.get(2).and_then(|x| x.usize()).ok_or("bad wire endpoint")?;
            Ok(DraftTo::Node(n, leg))
        }
        _ => Err("bad wire endpoint".into()),
    }
}

fn parse_ty(v: Option<&Json>, structs: &StructLib) -> Result<ItemType, String> {
    let id = v.and_then(|x| x.u64()).ok_or("missing item type")?;
    let ty = ItemType(id as u32);
    if structs.contains(ty) {
        Ok(ty)
    } else {
        Err(format!("unknown item type id {id}"))
    }
}

fn parse_pairs(
    v: Option<&Json>,
    structs: &StructLib,
) -> Result<Vec<(ItemType, u64)>, String> {
    v.and_then(|x| x.arr())
        .ok_or("missing recipe legs")?
        .iter()
        .map(|pair| {
            let a = pair.arr().ok_or("bad recipe leg")?;
            let ty = parse_ty(a.first(), structs)?;
            let amt = a.get(1).and_then(|x| x.u64()).ok_or("bad recipe amount")?;
            Ok((ty, amt))
        })
        .collect()
}

fn parse_draft_value(v: &Json, structs: &StructLib, depth: usize) -> Result<Draft, String> {
    if depth > 32 {
        return Err("modules nested too deep".into());
    }
    let mut d = Draft { types: demo::palette(), ..Default::default() };

    for (i, input) in v
        .get("inputs")
        .and_then(|x| x.arr())
        .unwrap_or(&[])
        .iter()
        .enumerate()
    {
        let rate = input.get("rate").and_then(|r| r.arr()).ok_or("source needs a rate")?;
        d.inputs.push(DraftInput {
            ty: parse_ty(input.get("ty"), structs)?,
            label: input
                .get("label")
                .and_then(|l| l.str())
                .map(|l| l.to_string())
                .unwrap_or_else(|| format!("source {i}")),
            rate: (
                rate.first().and_then(|x| x.u64()).ok_or("bad rate")?,
                rate.get(1).and_then(|x| x.u64()).ok_or("bad rate")?,
            ),
        });
    }
    for (o, out) in v
        .get("outputs")
        .and_then(|x| x.arr())
        .unwrap_or(&[])
        .iter()
        .enumerate()
    {
        d.outputs.push(DraftOutput {
            ty: parse_ty(out.get("ty"), structs)?,
            label: out
                .get("label")
                .and_then(|l| l.str())
                .map(|l| l.to_string())
                .unwrap_or_else(|| format!("out {o}")),
        });
    }
    for (n, node) in v
        .get("nodes")
        .and_then(|x| x.arr())
        .unwrap_or(&[])
        .iter()
        .enumerate()
    {
        let label = node
            .get("label")
            .and_then(|l| l.str())
            .map(|l| l.to_string())
            .unwrap_or_else(|| format!("machine {n}"));
        match node.get("kind").and_then(|k| k.str()) {
            Some("recipe") => d.nodes.push(DraftNode::Recipe {
                label,
                consume: parse_pairs(node.get("consume"), structs)?,
                produce: parse_pairs(node.get("produce"), structs)?,
                latency: node.get("latency").and_then(|x| x.u64()).unwrap_or(1),
            }),
            Some("priority") => d.nodes.push(DraftNode::Priority {
                label,
                item: parse_ty(node.get("item"), structs)?,
                token: parse_ty(node.get("token"), structs)?,
            }),
            Some("module") => d.nodes.push(DraftNode::Module {
                label,
                draft: Box::new(parse_draft_value(
                    node.get("draft").ok_or("module missing its draft")?,
                    structs,
                    depth + 1,
                )?),
            }),
            Some(kind @ ("weld" | "rot" | "split" | "belt")) => {
                let latency = node.get("latency").and_then(|x| x.u64()).unwrap_or(1);
                let op = match kind {
                    "weld" => BuildOp::Weld {
                        dx: node.get("dx").and_then(|x| x.i64()).unwrap_or(1) as i32,
                        dy: node.get("dy").and_then(|x| x.i64()).unwrap_or(0) as i32,
                    },
                    "rot" => BuildOp::Rot,
                    "split" => BuildOp::Split,
                    _ => BuildOp::Belt,
                };
                d.nodes.push(DraftNode::Builder { label, op, latency });
            }
            _ => return Err(format!("node {n}: unknown kind")),
        }
    }
    for w in v.get("wires").and_then(|x| x.arr()).unwrap_or(&[]) {
        d.wires.push((
            parse_endpoint_from(w.get("from").ok_or("wire missing from")?)?,
            parse_endpoint_to(w.get("to").ok_or("wire missing to")?)?,
        ));
    }
    for m in v.get("markings").and_then(|x| x.arr()).unwrap_or(&[]) {
        let to = parse_endpoint_to(m.get("to").ok_or("marking missing port")?)?;
        let n = m.get("n").and_then(|x| x.u64()).ok_or("marking missing count")?;
        d.markings.push((to, n));
    }
    if let Some(pos) = v.get("pos") {
        let read = |key: &str| -> Vec<(i32, i32)> {
            pos.get(key)
                .and_then(|x| x.arr())
                .unwrap_or(&[])
                .iter()
                .filter_map(|p| {
                    let a = p.arr()?;
                    Some((a.first()?.i64()? as i32, a.get(1)?.i64()? as i32))
                })
                .collect()
        };
        d.input_pos = read("inputs");
        d.node_pos = read("nodes");
        d.output_pos = read("outputs");
    }
    Ok(d)
}

fn parse_draft(body: &str, structs: &StructLib) -> Result<Draft, String> {
    let v = json::parse(body).map_err(|e| format!("bad JSON: {e}"))?;
    parse_draft_value(&v, structs, 0)
}

// ---------------------------------------------------------------------------
// HTTP
// ---------------------------------------------------------------------------

fn route(app: &mut App, method: &str, path: &str, body: &str) -> (String, &'static str, String) {
    let (route, query) = path.split_once('?').unwrap_or((path, ""));
    let param = |key: &str| -> Option<u64> {
        query.split('&').find_map(|kv| {
            let (k, v) = kv.split_once('=')?;
            (k == key).then(|| v.parse().ok()).flatten()
        })
    };
    match (method, route) {
        ("GET", "/") => ("200 OK".into(), "text/html; charset=utf-8", INDEX_HTML.to_string()),
        ("GET", "/api/scene") => {
            ("200 OK".into(), "application/json", scene_json(app))
        }
        ("GET", "/api/frames") => {
            let from = param("from").unwrap_or(0);
            let n = param("n").unwrap_or(32);
            ("200 OK".into(), "application/json", frames_json(&app.current, from, n))
        }
        ("POST", "/api/compile") => {
            let result = parse_draft(body, &app.structs)
                .and_then(|draft| compile(&mut app.lib, &mut app.structs, draft));
            match result {
                Ok(compiled) => {
                    app.current = compiled;
                    let body = format!("{{\"ok\":true,\"scene\":{}}}", scene_json(app));
                    ("200 OK".into(), "application/json", body)
                }
                Err(e) => (
                    "200 OK".into(),
                    "application/json",
                    format!("{{\"ok\":false,\"error\":\"{}\"}}", esc(&e)),
                ),
            }
        }
        _ => ("404 Not Found".into(), "text/plain", "not found".into()),
    }
}

fn handle(mut stream: TcpStream, app: &mut App) {
    stream
        .set_read_timeout(Some(std::time::Duration::from_secs(2)))
        .ok();
    let mut buf = Vec::with_capacity(8192);
    let mut chunk = [0u8; 4096];
    let header_end = loop {
        match stream.read(&mut chunk) {
            Ok(0) => break None,
            Ok(k) => {
                buf.extend_from_slice(&chunk[..k]);
                if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                    break Some(pos + 4);
                }
                if buf.len() > 1 << 20 {
                    return;
                }
            }
            Err(_) => return,
        }
    };
    let Some(header_end) = header_end else { return };
    let head = String::from_utf8_lossy(&buf[..header_end]).to_string();
    let mut first = head.lines().next().unwrap_or("").split_whitespace();
    let method = first.next().unwrap_or("GET").to_string();
    let path = first.next().unwrap_or("/").to_string();
    let content_length: usize = head
        .lines()
        .find_map(|l| {
            let (k, v) = l.split_once(':')?;
            k.eq_ignore_ascii_case("content-length")
                .then(|| v.trim().parse().ok())
                .flatten()
        })
        .unwrap_or(0);
    while buf.len() < header_end + content_length {
        match stream.read(&mut chunk) {
            Ok(0) => break,
            Ok(k) => buf.extend_from_slice(&chunk[..k]),
            Err(_) => return,
        }
    }
    let body = String::from_utf8_lossy(&buf[header_end..]).to_string();
    let (status, ctype, out) = route(app, &method, &path, &body);
    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\n\
         Connection: close\r\n\r\n{out}",
        out.len()
    );
    stream.write_all(response.as_bytes()).ok();
}

fn main() {
    let port: u16 = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(8470);

    let mut app = App::new();

    let listener =
        TcpListener::bind(("127.0.0.1", port)).expect("bind GUI port (pass another as arg)");
    println!("hashtorio GUI \u{2192} http://127.0.0.1:{port}");
    for stream in listener.incoming().flatten() {
        handle(stream, &mut app);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_app() -> App {
        App::new()
    }

    #[test]
    fn scene_shows_the_sealed_module() {
        let mut app = test_app();
        let (_, _, body) = route(&mut app, "GET", "/api/scene", "");
        assert!(body.contains("chassis store"));
        assert!(body.contains("\"kind\":\"module\""));
        assert!(body.contains("\"draft\""), "module carries its sub-draft");
        // Frames: module legs report null occupancy (interior is sealed).
        let (_, _, frames) = route(&mut app, "GET", "/api/frames?from=50&n=1", "");
        assert!(frames.contains("null"), "{frames}");
    }

    #[test]
    fn module_draft_round_trips_through_compile() {
        let mut app = test_app();
        let sub = r#"{"inputs":[{"ty":2,"label":"g","rate":[1,1]}],
            "outputs":[{"ty":2,"label":"o"}],
            "nodes":[{"kind":"recipe","label":"belt","consume":[[2,1]],
                      "produce":[[2,1]],"latency":2}],
            "wires":[{"from":["in",0],"to":["node",0,0]},
                     {"from":["node",0,0],"to":["out",0]}],
            "markings":[]}"#;
        let top = format!(
            r#"{{"inputs":[{{"ty":0,"label":"mine","rate":[1,1]}}],
            "outputs":[{{"ty":2,"label":"out"}}],
            "nodes":[{{"kind":"recipe","label":"press","consume":[[0,2]],
                       "produce":[[2,1]],"latency":3}},
                     {{"kind":"module","label":"boxed belt","draft":{sub}}}],
            "wires":[{{"from":["in",0],"to":["node",0,0]}},
                     {{"from":["node",0,0],"to":["node",1,0]}},
                     {{"from":["node",1,0],"to":["out",0]}}],
            "markings":[]}}"#
        );
        let (_, _, body) = route(&mut app, "POST", "/api/compile", &top);
        assert!(body.contains("\"ok\":true"), "{body}");
        assert!(body.contains("\"rate\":[1,2]"), "{body}");
        assert!(body.contains("boxed belt"));
    }

    #[test]
    fn loops_through_module_boundaries_refuse_kindly() {
        let mut app = test_app();
        let sub = r#"{"inputs":[{"ty":2,"label":"g","rate":[1,1]}],
            "outputs":[{"ty":2,"label":"o"}],
            "nodes":[{"kind":"recipe","label":"belt","consume":[[2,1]],
                      "produce":[[2,1]],"latency":1}],
            "wires":[{"from":["in",0],"to":["node",0,0]},
                     {"from":["node",0,0],"to":["out",0]}],
            "markings":[]}"#;
        let top = format!(
            r#"{{"inputs":[],"outputs":[],
            "nodes":[{{"kind":"module","label":"loopy","draft":{sub}}}],
            "wires":[{{"from":["node",0,0],"to":["node",0,0]}}],
            "markings":[{{"to":["node",0,0],"n":3}}]}}"#
        );
        let (_, _, body) = route(&mut app, "POST", "/api/compile", &top);
        assert!(body.contains("module boundary"), "{body}");
        // The demo survives the refusal.
        let (_, _, scene) = route(&mut app, "GET", "/api/scene", "");
        assert!(scene.contains("chassis store"));
    }
}
