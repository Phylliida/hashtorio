//! The hashtorio GUI: a zero-dependency HTTP server (std::net only) feeding
//! a single-page canvas frontend — now with editing.
//!
//! GET  /            the app
//! GET  /api/scene   current compiled factory: topology + spec + audit
//! GET  /api/frames  batched per-tick data (O(1) reads of counting maps)
//! POST /api/compile a draft blueprint; on success it becomes the current
//!                   factory, on failure the error is a friendly refusal —
//!                   the engine teaching its own rules.

mod json;

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};

use hashtorio::demo;
use hashtorio::draft::{Draft, DraftFrom, DraftInput, DraftNode, DraftOutput, DraftTo};
use hashtorio::eval::{EvalError, Evaluator};
use hashtorio::net::{ItemType, Library};
use hashtorio::render::{Edge, EdgeFrom, EdgeTo, Scene};
use hashtorio::report::{Audit, Summary};
use json::Json;

const INDEX_HTML: &str = include_str!("../../../gui/index.html");
const MAX_BATCH: u64 = 128;
/// Compiles should feel instant; refusals past this horizon are honest.
const GUI_HORIZON: u64 = 4096;

struct Compiled {
    draft: Draft,
    scene: Scene,
    summary: Summary,
    audit: Audit,
    /// Draft wire index -> scene edge index (interning sorts sources).
    edge_map: Vec<usize>,
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
        other => format!("evaluation failed: {other:?}"),
    }
}

fn compile(lib: &mut Library, draft: Draft) -> Result<Compiled, String> {
    let (id, flows) = draft.build(lib)?;
    let mut ev = Evaluator::new(lib);
    ev.horizon = GUI_HORIZON;
    let summary = ev.summarize(id, &flows).map_err(|e| friendly_eval_error(&e))?;
    let audit = ev.audit(id, &flows).map_err(|e| friendly_eval_error(&e))?;
    let trace = ev
        .trace_flattened(id, &flows)
        .map_err(|e| friendly_eval_error(&e))?;
    let type_names: Vec<(ItemType, &str)> =
        draft.types.iter().map(|(t, n)| (*t, n.as_str())).collect();
    let node_labels = draft.nodes.iter().map(|n| n.label().to_string()).collect();
    let out_labels = draft.outputs.iter().map(|o| o.label.clone()).collect();
    let scene = Scene::new(lib, trace, node_labels, out_labels, &type_names);

    // Interning sorts wire sources, so scene edges may be reordered
    // relative to draft wires; match them up by endpoints.
    let matches = |e: &Edge, from: &DraftFrom, to: &DraftTo| {
        let f = match (e.from, from) {
            (EdgeFrom::Input(a), DraftFrom::Input(b)) => a == *b,
            (EdgeFrom::Node { node, leg }, DraftFrom::Node(n, l)) => {
                node == *n && leg == *l
            }
            _ => false,
        };
        let t = match (e.to, to) {
            (EdgeTo::Output(a), DraftTo::Output(b)) => a == *b,
            (EdgeTo::Node { node, leg }, DraftTo::Node(n, l)) => node == *n && leg == *l,
            _ => false,
        };
        f && t
    };
    let edge_map = draft
        .wires
        .iter()
        .map(|(from, to)| {
            scene
                .edges()
                .iter()
                .position(|e| matches(e, from, to))
                .ok_or_else(|| "internal: draft wire missing from scene".to_string())
        })
        .collect::<Result<Vec<_>, _>>()?;

    Ok(Compiled { draft, scene, summary, audit, edge_map })
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

fn scene_json(c: &Compiled) -> String {
    let d = &c.draft;
    let marking_of = |to: DraftTo| -> u64 {
        d.markings
            .iter()
            .filter(|(t, _)| *t == to)
            .map(|(_, m)| *m)
            .sum()
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
        .map(|(n, node)| {
            let legs: Vec<String> =
                node.in_types().iter().map(|t| t.0.to_string()).collect();
            let outs: Vec<String> =
                node.out_types().iter().map(|t| t.0.to_string()).collect();
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
            };
            format!(
                "{{{core},\"label\":\"{}\",\"legs\":[{}],\"outs\":[{}],\"markings\":[{}]}}",
                esc(node.label()),
                legs.join(","),
                outs.join(","),
                markings.join(",")
            )
        })
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
    let s = &c.scene;
    let fired: Vec<String> =
        (0..s.node_count()).map(|n| s.fired(n, t).to_string()).collect();
    let occ: Vec<String> = (0..s.node_count())
        .map(|n| {
            let legs: Vec<String> = (0..s.node_in_type_names(n).len())
                .map(|l| s.occupancy(n, l, t).to_string())
                .collect();
            format!("[{}]", legs.join(","))
        })
        .collect();
    // Flows in draft-wire order, via the edge map.
    let flow: Vec<String> = c
        .edge_map
        .iter()
        .map(|&ei| s.edge_flow(ei, t).to_string())
        .collect();
    let outs: Vec<String> = (0..s.output_count())
        .map(|o| {
            let total = s.delivered(o, t);
            let delta = total - if t == 0 { 0 } else { s.delivered(o, t - 1) };
            format!("[{total},{delta}]")
        })
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
// JSON in: the draft payload
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

fn parse_draft(body: &str) -> Result<Draft, String> {
    let v = json::parse(body).map_err(|e| format!("bad JSON: {e}"))?;
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
            let result = parse_draft(body)
                .and_then(|draft| compile(&mut app.lib, draft));
            match result {
                Ok(compiled) => {
                    app.current = compiled;
                    let body = format!("{{\"ok\":true,\"scene\":{}}}", scene_json(&app.current));
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
    fn get_routes_serve_scene_and_frames() {
        let mut app = test_app();
        let (status, _, body) = route(&mut app, "GET", "/api/scene", "");
        assert_eq!(status, "200 OK");
        assert!(body.contains("gear assembler"));
        assert!(body.contains("\"types\""));
        let (_, _, body) = route(&mut app, "GET", "/api/frames?from=100&n=2", "");
        assert!(body.contains("\"from\":100"));
        let (_, _, body) = route(&mut app, "GET", "/", "");
        assert!(body.contains("<canvas"));
    }

    #[test]
    fn compile_swaps_the_current_factory() {
        let mut app = test_app();
        let draft = r#"{"inputs":[{"ty":0,"label":"mine","rate":[1,1]}],
            "outputs":[{"ty":2,"label":"gears out"}],
            "nodes":[{"kind":"recipe","label":"press","consume":[[0,2]],
                      "produce":[[2,1]],"latency":3}],
            "wires":[{"from":["in",0],"to":["node",0,0]},
                     {"from":["node",0,0],"to":["out",0]}],
            "markings":[]}"#;
        let (_, _, body) = route(&mut app, "POST", "/api/compile", draft);
        assert!(body.contains("\"ok\":true"), "{body}");
        assert!(body.contains("\"rate\":[1,2]"), "{body}");
        // The current scene really swapped.
        let (_, _, scene) = route(&mut app, "GET", "/api/scene", "");
        assert!(scene.contains("gears out"));
        // Frames follow the new factory: first gear lands at t=5.
        let (_, _, frames) = route(&mut app, "GET", "/api/frames?from=5&n=1", "");
        assert!(frames.contains("\"outs\":[[1,1]]"), "{frames}");
    }

    #[test]
    fn compile_refusals_are_friendly() {
        let mut app = test_app();
        // Copying an output.
        let dup = r#"{"inputs":[{"ty":0,"rate":[1,1]}],
            "outputs":[{"ty":0,"label":"a"},{"ty":0,"label":"b"}],"nodes":[],
            "wires":[{"from":["in",0],"to":["out",0]},{"from":["in",0],"to":["out",1]}],
            "markings":[]}"#;
        let (_, _, body) = route(&mut app, "POST", "/api/compile", dup);
        assert!(body.contains("can't be copied"), "{body}");
        // Zeno loop.
        let zeno = r#"{"inputs":[],"outputs":[],
            "nodes":[{"kind":"recipe","label":"z","consume":[[0,1]],
                      "produce":[[0,1]],"latency":0}],
            "wires":[{"from":["node",0,0],"to":["node",0,0]}],
            "markings":[{"to":["node",0,0],"n":1}]}"#;
        let (_, _, body) = route(&mut app, "POST", "/api/compile", zeno);
        assert!(body.contains("zero-latency"), "{body}");
        // A refusal must not clobber the current factory.
        let (_, _, scene) = route(&mut app, "GET", "/api/scene", "");
        assert!(scene.contains("gear assembler"));
    }
}
