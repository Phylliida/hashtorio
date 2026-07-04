//! The hashtorio GUI: a zero-dependency HTTP server (std::net only) feeding
//! a single-page canvas frontend. The engine stays the single source of
//! truth — the browser only ever asks "describe the scene" once and then
//! "what happened at tick t", each answer an O(1) read of the exact
//! counting maps. Scrubbing and warping cost the same as playing.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};

use hashtorio::demo;
use hashtorio::eval::Evaluator;
use hashtorio::net::Library;
use hashtorio::render::{EdgeFrom, EdgeTo, Scene};
use hashtorio::report::{Audit, Summary};

const INDEX_HTML: &str = include_str!("../../gui/index.html");
const MAX_BATCH: u64 = 128;

struct App {
    scene: Scene,
    summary: Summary,
    audit: Audit,
    in_labels: Vec<String>,
}

fn esc(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

fn rate_json(r: (u64, u64)) -> String {
    format!("[{},{}]", r.0, r.1)
}

impl App {
    fn scene_json(&self) -> String {
        let s = &self.scene;
        let nodes: Vec<String> = (0..s.node_count())
            .map(|n| {
                let legs: Vec<String> = s
                    .node_in_type_names(n)
                    .iter()
                    .map(|t| format!("\"{}\"", esc(t)))
                    .collect();
                format!(
                    "{{\"label\":\"{}\",\"kind\":\"{}\",\"legs\":[{}]}}",
                    esc(s.node_label(n)),
                    s.node_kind(n),
                    legs.join(",")
                )
            })
            .collect();
        let edges: Vec<String> = s
            .edges()
            .iter()
            .map(|e| {
                let from = match e.from {
                    EdgeFrom::Input(i) => format!("[\"in\",{i}]"),
                    EdgeFrom::Node { node, leg } => format!("[\"node\",{node},{leg}]"),
                };
                let to = match e.to {
                    EdgeTo::Node { node, leg } => format!("[\"node\",{node},{leg}]"),
                    EdgeTo::Output(o) => format!("[\"out\",{o}]"),
                };
                format!(
                    "{{\"from\":{from},\"to\":{to},\"ty\":\"{}\"}}",
                    esc(s.type_label(e.ty))
                )
            })
            .collect();
        let ins: Vec<String> = self
            .in_labels
            .iter()
            .map(|l| format!("\"{}\"", esc(l)))
            .collect();
        let outs: Vec<String> = (0..s.output_count())
            .map(|o| format!("\"{}\"", esc(s.output_label(o))))
            .collect();
        let spec: Vec<String> = self
            .summary
            .outputs
            .iter()
            .enumerate()
            .map(|(o, p)| {
                format!(
                    "{{\"name\":\"{}\",\"rate\":{},\"first\":{}}}",
                    esc(s.output_label(o)),
                    rate_json(p.rate),
                    p.first.map(|t| t.to_string()).unwrap_or("null".into())
                )
            })
            .collect();
        let audit: Vec<String> = self
            .audit
            .types
            .iter()
            .map(|r| {
                format!(
                    "{{\"ty\":\"{}\",\"injected\":{},\"minted\":{},\"consumed\":{},\
                     \"delivered\":{},\"discarded\":{},\"accumulating\":{}}}",
                    esc(s.type_label(r.ty)),
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
            "{{\"nodes\":[{}],\"edges\":[{}],\"inputs\":[{}],\"outputs\":[{}],\
             \"spec\":[{}],\"audit\":[{}]}}",
            nodes.join(","),
            edges.join(","),
            ins.join(","),
            outs.join(","),
            spec.join(","),
            audit.join(",")
        )
    }

    fn frame_json(&self, t: u64) -> String {
        let s = &self.scene;
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
        let flow: Vec<String> =
            (0..s.edges().len()).map(|i| s.edge_flow(i, t).to_string()).collect();
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

    fn frames_json(&self, from: u64, n: u64) -> String {
        let n = n.clamp(1, MAX_BATCH);
        let frames: Vec<String> = (from..from + n).map(|t| self.frame_json(t)).collect();
        format!("{{\"from\":{from},\"frames\":[{}]}}", frames.join(","))
    }

    /// Route a request path to (status line, content type, body).
    fn route(&self, path: &str) -> (&'static str, &'static str, String) {
        let (route, query) = path.split_once('?').unwrap_or((path, ""));
        let param = |key: &str| -> Option<u64> {
            query.split('&').find_map(|kv| {
                let (k, v) = kv.split_once('=')?;
                (k == key).then(|| v.parse().ok()).flatten()
            })
        };
        match route {
            "/" => ("200 OK", "text/html; charset=utf-8", INDEX_HTML.to_string()),
            "/api/scene" => ("200 OK", "application/json", self.scene_json()),
            "/api/frames" => {
                let from = param("from").unwrap_or(0);
                let n = param("n").unwrap_or(32);
                ("200 OK", "application/json", self.frames_json(from, n))
            }
            _ => ("404 Not Found", "text/plain", "not found".into()),
        }
    }
}

fn handle(mut stream: TcpStream, app: &App) {
    stream
        .set_read_timeout(Some(std::time::Duration::from_secs(2)))
        .ok();
    let mut buf = [0u8; 8192];
    let mut len = 0usize;
    // Read until end of headers (GET requests carry no body).
    while len < buf.len() {
        match stream.read(&mut buf[len..]) {
            Ok(0) => break,
            Ok(k) => {
                len += k;
                if buf[..len].windows(4).any(|w| w == b"\r\n\r\n") {
                    break;
                }
            }
            Err(_) => return,
        }
    }
    let request = String::from_utf8_lossy(&buf[..len]);
    let path = request
        .lines()
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .unwrap_or("/");
    let (status, ctype, body) = app.route(path);
    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\n\
         Connection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(response.as_bytes()).ok();
}

fn main() {
    let port: u16 = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(8470);

    let mut lib = Library::new();
    let d = demo::build(&mut lib);
    let mut ev = Evaluator::new(&lib);
    let summary = ev.summarize(d.id, &d.inputs).expect("demo summarizes");
    let audit = ev.audit(d.id, &d.inputs).expect("demo audits");
    let trace = ev.trace_flattened(d.id, &d.inputs).expect("demo traces");
    let type_names = d.type_names.clone();
    let app = App {
        scene: Scene::new(&lib, trace, d.node_labels, d.out_labels, &type_names),
        summary,
        audit,
        in_labels: d.in_labels,
    };

    let listener =
        TcpListener::bind(("127.0.0.1", port)).expect("bind GUI port (pass another as arg)");
    println!("hashtorio GUI \u{2192} http://127.0.0.1:{port}");
    for stream in listener.incoming().flatten() {
        handle(stream, &app);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_app() -> App {
        let mut lib = Library::new();
        let d = demo::build(&mut lib);
        let mut ev = Evaluator::new(&lib);
        let summary = ev.summarize(d.id, &d.inputs).unwrap();
        let audit = ev.audit(d.id, &d.inputs).unwrap();
        let trace = ev.trace_flattened(d.id, &d.inputs).unwrap();
        let type_names = d.type_names.clone();
        App {
            scene: Scene::new(&lib, trace, d.node_labels, d.out_labels, &type_names),
            summary,
            audit,
            in_labels: d.in_labels,
        }
    }

    #[test]
    fn routes_serve_scene_and_frames() {
        let app = test_app();
        let (status, _, body) = app.route("/api/scene");
        assert_eq!(status, "200 OK");
        assert!(body.contains("gear assembler"));
        assert!(body.contains("\"edges\""));
        let (status, _, body) = app.route("/api/frames?from=100&n=2");
        assert_eq!(status, "200 OK");
        assert!(body.contains("\"from\":100"));
        assert!(body.contains("\"flow\""));
        let (status, _, body) = app.route("/");
        assert_eq!(status, "200 OK");
        assert!(body.contains("<canvas"));
        let (status, ..) = app.route("/nope");
        assert_eq!(status, "404 Not Found");
    }
}
