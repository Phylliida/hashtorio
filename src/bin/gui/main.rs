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
use hashtorio::draft::{Draft, DraftFrom, DraftInput, DraftNode, DraftOutput, DraftTo};
use hashtorio::eval::{EvalError, Evaluator};
use hashtorio::net::{ItemType, Library};
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
    summary: Summary,
    audit: Audit,
}

struct App {
    lib: Library,
    current: Compiled,
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

fn compile(lib: &mut Library, draft: Draft) -> Result<Compiled, String> {
    let (id, input_flows) = draft.build(lib)?;
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
    let mut supplies = Vec::with_capacity(draft.nodes.len());
    let mut consumed = Vec::with_capacity(draft.nodes.len());
    for (n, node) in draft.nodes.iter().enumerate() {
        let legs = node.in_types().len();
        let sup: Vec<Counting> = (0..legs)
            .map(|l| {
                let mut acc = Counting::constant(marking_of(DraftTo::Node(n, l)));
                for (from, to) in &draft.wires {
                    if *to == DraftTo::Node(n, l) {
                        acc = acc.add(source_counting(from));
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

fn node_json(d: &Draft, n: usize, node: &DraftNode) -> String {
    let marking_of = |to: DraftTo| -> u64 {
        d.markings.iter().filter(|(t, _)| *t == to).map(|(_, m)| *m).sum()
    };
    let legs: Vec<String> = node.in_types().iter().map(|t| t.0.to_string()).collect();
    let outs: Vec<String> = node.out_types().iter().map(|t| t.0.to_string()).collect();
    let markings: Vec<String> = (0..node.in_types().len())
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
    };
    format!(
        "{{{core},\"label\":\"{}\",\"legs\":[{}],\"outs\":[{}],\"markings\":[{}]}}",
        esc(node.label()),
        legs.join(","),
        outs.join(","),
        markings.join(",")
    )
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
        .map(|(n, node)| node_json(d, n, node))
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
    format!(
        "{{\"inputs\":[{}],\"outputs\":[{}],\"nodes\":[{}],\"wires\":[{}],\"markings\":[{}]}}",
        inputs.join(","),
        outputs.join(","),
        nodes.join(","),
        wires.join(","),
        markings.join(",")
    )
}

fn scene_json(c: &Compiled) -> String {
    let d = &c.draft;
    let marking_of = |to: DraftTo| -> u64 {
        d.markings.iter().filter(|(t, _)| *t == to).map(|(_, m)| *m).sum()
    };
    let types: Vec<String> = d
        .types
        .iter()
        .map(|(t, n)| format!("{{\"id\":{},\"name\":\"{}\"}}", t.0, esc(n)))
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
        .map(|(n, node)| node_json(d, n, node))
        .collect();
    let edges: Vec<String> = d
        .wires
        .iter()
        .map(|(from, to)| {
            let ty = d.sink_type(*to).map(|t| t.0).unwrap_or(0);
            format!(
                "{{\"from\":{},\"to\":{},\"ty\":{ty}}}",
                from_json(from),
                to_json(to)
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
                esc(d.type_name(r.ty)),
                rate_json(r.injected),
                rate_json(r.minted),
                rate_json(r.consumed),
                rate_json(r.delivered),
                rate_json(r.discarded),
                rate_json(r.accumulating),
            )
        })
        .collect();
    format!(
        "{{\"types\":[{}],\"inputs\":[{}],\"outputs\":[{}],\"nodes\":[{}],\
         \"edges\":[{}],\"spec\":[{}],\"audit\":[{}]}}",
        types.join(","),
        inputs.join(","),
        outputs.join(","),
        nodes.join(","),
        edges.join(","),
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
    let flow: Vec<String> = c
        .draft
        .wires
        .iter()
        .map(|(from, _)| {
            let cnt = match from {
                DraftFrom::Input(i) => &c.input_flows[*i],
                DraftFrom::Node(n, l) => &c.node_outs[*n][*l],
            };
            delta(cnt).to_string()
        })
        .collect();
    let outs: Vec<String> = c
        .outputs
        .iter()
        .map(|cnt| format!("[{},{}]", cnt.eval(t), delta(cnt)))
        .collect();
    format!(
        "{{\"fired\":[{}],\"occ\":[{}],\"flow\":[{}],\"outs\":[{}]}}",
        fired.join(","),
        occ.join(","),
        flow.join(","),
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

fn parse_ty(v: Option<&Json>, palette: &[(ItemType, String)]) -> Result<ItemType, String> {
    let id = v.and_then(|x| x.u64()).ok_or("missing item type")?;
    let ty = ItemType(id as u32);
    if palette.iter().any(|(t, _)| *t == ty) {
        Ok(ty)
    } else {
        Err(format!("unknown item type id {id}"))
    }
}

fn parse_pairs(
    v: Option<&Json>,
    palette: &[(ItemType, String)],
) -> Result<Vec<(ItemType, u64)>, String> {
    v.and_then(|x| x.arr())
        .ok_or("missing recipe legs")?
        .iter()
        .map(|pair| {
            let a = pair.arr().ok_or("bad recipe leg")?;
            let ty = parse_ty(a.first(), palette)?;
            let amt = a.get(1).and_then(|x| x.u64()).ok_or("bad recipe amount")?;
            Ok((ty, amt))
        })
        .collect()
}

fn parse_draft_value(v: &Json, depth: usize) -> Result<Draft, String> {
    if depth > 32 {
        return Err("modules nested too deep".into());
    }
    let palette = demo::palette();
    let mut d = Draft { types: palette.clone(), ..Default::default() };

    for (i, input) in v
        .get("inputs")
        .and_then(|x| x.arr())
        .unwrap_or(&[])
        .iter()
        .enumerate()
    {
        let rate = input.get("rate").and_then(|r| r.arr()).ok_or("source needs a rate")?;
        d.inputs.push(DraftInput {
            ty: parse_ty(input.get("ty"), &palette)?,
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
            ty: parse_ty(out.get("ty"), &palette)?,
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
                consume: parse_pairs(node.get("consume"), &palette)?,
                produce: parse_pairs(node.get("produce"), &palette)?,
                latency: node.get("latency").and_then(|x| x.u64()).unwrap_or(1),
            }),
            Some("priority") => d.nodes.push(DraftNode::Priority {
                label,
                item: parse_ty(node.get("item"), &palette)?,
                token: parse_ty(node.get("token"), &palette)?,
            }),
            Some("module") => d.nodes.push(DraftNode::Module {
                label,
                draft: Box::new(parse_draft_value(
                    node.get("draft").ok_or("module missing its draft")?,
                    depth + 1,
                )?),
            }),
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
    Ok(d)
}

fn parse_draft(body: &str) -> Result<Draft, String> {
    let v = json::parse(body).map_err(|e| format!("bad JSON: {e}"))?;
    parse_draft_value(&v, 0)
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
            ("200 OK".into(), "application/json", scene_json(&app.current))
        }
        ("GET", "/api/frames") => {
            let from = param("from").unwrap_or(0);
            let n = param("n").unwrap_or(32);
            ("200 OK".into(), "application/json", frames_json(&app.current, from, n))
        }
        ("POST", "/api/compile") => {
            let result = parse_draft(body).and_then(|draft| compile(&mut app.lib, draft));
            match result {
                Ok(compiled) => {
                    app.current = compiled;
                    let body =
                        format!("{{\"ok\":true,\"scene\":{}}}", scene_json(&app.current));
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

    let mut lib = Library::new();
    let current = compile(&mut lib, demo::draft()).expect("demo compiles");
    let mut app = App { lib, current };

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
        let mut lib = Library::new();
        let current = compile(&mut lib, demo::draft()).unwrap();
        App { lib, current }
    }

    #[test]
    fn scene_shows_the_sealed_module() {
        let mut app = test_app();
        let (_, _, body) = route(&mut app, "GET", "/api/scene", "");
        assert!(body.contains("demand store"));
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
        assert!(scene.contains("demand store"));
    }
}
