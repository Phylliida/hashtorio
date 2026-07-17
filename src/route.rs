//! Deterministic belt routing on the grid.
//!
//! A placed belt is a real path of cells, and the path is *semantic*:
//! its length is the wire's latency and its belt-segment cost (G0 of
//! DESIGN-motion.md; P2 of the spatialization arc). The router is 4-dir
//! A* with a turn penalty so belts prefer long straight runs, dodging
//! machine footprints.
//!
//! Determinism matters: routes feed latencies feed NetIds feed the memo
//! caches. All search state is kept in coordinates *relative to the start
//! cell*, with lexicographic tie-breaking, so equal drafts route equally
//! and translated drafts route to translated paths (the covariant twin of
//! the translation-invariance pin in `draft.rs`).

use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap, HashSet};

/// Cost of one cell step, in tenths (integer arithmetic, no floats).
const STEP: u64 = 10;
/// Extra cost of a 90° turn: belts prefer straight runs.
const TURN: u64 = 4;
/// Search budget; a hemmed-in wire falls back to the L-path.
const MAX_POPS: usize = 20_000;

const DX: [i32; 4] = [1, 0, -1, 0];
const DY: [i32; 4] = [0, 1, 0, -1];

/// Route from `a` to `b` around `blocked` cells, starting eastbound (the
/// direction out-ports face — belts leave a machine heading east). See
/// [`route_free`] for direction-free starts.
pub fn route(a: (i32, i32), b: (i32, i32), blocked: &HashSet<(i32, i32)>) -> Vec<(i32, i32)> {
    route_from(a, b, blocked, Some(0))
}

/// Route with no initial heading — a trundling machine may step any way
/// first. (With an eastbound seed and the no-U-turn rule, a walker could
/// never take its first step west: it would spiral at its own doorstep.
/// The commuting-workshop test caught exactly that.)
pub fn route_free(a: (i32, i32), b: (i32, i32), blocked: &HashSet<(i32, i32)>) -> Vec<(i32, i32)> {
    route_from(a, b, blocked, None)
}

/// Route from `a` to `b` around `blocked` cells. Returns the full cell
/// path including both endpoints (`a == b` gives a single cell). The
/// endpoints themselves are never treated as blocked; if no clear path
/// exists within budget, the Manhattan L-path is returned — same length
/// the old latency rule assumed, so hemmed wires degrade gracefully.
fn route_from(
    a: (i32, i32),
    b: (i32, i32),
    blocked: &HashSet<(i32, i32)>,
    start_dir: Option<u8>,
) -> Vec<(i32, i32)> {
    if a == b {
        return vec![a];
    }
    // Everything below is relative to `a` (translation covariance).
    let rel = |p: (i32, i32)| (p.0 - a.0, p.1 - a.1);
    let bt = rel(b);
    let mut x0 = 0.min(bt.0);
    let mut y0 = 0.min(bt.1);
    let mut x1 = 0.max(bt.0);
    let mut y1 = 0.max(bt.1);
    let mut obs: HashSet<(i32, i32)> = HashSet::new();
    for &c in blocked {
        let r = rel(c);
        x0 = x0.min(r.0);
        y0 = y0.min(r.1);
        x1 = x1.max(r.0);
        y1 = y1.max(r.1);
        obs.insert(r);
    }
    x0 -= 5;
    y0 -= 5;
    x1 += 5;
    y1 += 5;

    let h = |x: i32, y: i32| ((x - bt.0).abs() + (y - bt.1).abs()) as u64 * STEP;
    // Heap entries: (f, x, y, dir) — lexicographic tie-break on relative
    // coordinates keeps the search fully deterministic.
    let mut heap: BinaryHeap<Reverse<(u64, i32, i32, u8)>> = BinaryHeap::new();
    let mut g: HashMap<(i32, i32, u8), u64> = HashMap::new();
    let mut par: HashMap<(i32, i32, u8), (i32, i32, u8)> = HashMap::new();
    match start_dir {
        Some(d) => {
            g.insert((0, 0, d), 0);
            heap.push(Reverse((h(0, 0), 0, 0, d)));
        }
        None => {
            for d in 0..4u8 {
                g.insert((0, 0, d), 0);
                heap.push(Reverse((h(0, 0), 0, 0, d)));
            }
        }
    }
    let mut goal: Option<(i32, i32, u8)> = None;
    let mut pops = 0;
    while let Some(Reverse((f, x, y, d))) = heap.pop() {
        pops += 1;
        if pops > MAX_POPS {
            break;
        }
        let gc = g[&(x, y, d)];
        if f > gc + h(x, y) {
            continue; // stale entry
        }
        if (x, y) == bt {
            goal = Some((x, y, d));
            break;
        }
        for nd in 0..4u8 {
            if nd == (d + 2) % 4 {
                continue; // no U-turns
            }
            let nx = x + DX[nd as usize];
            let ny = y + DY[nd as usize];
            if nx < x0 || nx > x1 || ny < y0 || ny > y1 {
                continue;
            }
            if obs.contains(&(nx, ny)) && (nx, ny) != bt {
                continue;
            }
            let ng = gc + STEP + if nd == d { 0 } else { TURN };
            let key = (nx, ny, nd);
            if ng < *g.get(&key).unwrap_or(&u64::MAX) {
                g.insert(key, ng);
                par.insert(key, (x, y, d));
                heap.push(Reverse((ng + h(nx, ny), nx, ny, nd)));
            }
        }
    }
    let Some(goal) = goal else {
        // Hemmed in: the Manhattan L-path, straight out then over.
        let mut cells = vec![a];
        let sx = (b.0 - a.0).signum();
        let mut x = a.0;
        while x != b.0 {
            x += sx;
            cells.push((x, a.1));
        }
        let sy = (b.1 - a.1).signum();
        let mut y = a.1;
        while y != b.1 {
            y += sy;
            cells.push((b.0, y));
        }
        return cells;
    };
    let mut cells = Vec::new();
    let mut cur = goal;
    loop {
        let abs = (cur.0 + a.0, cur.1 + a.1);
        if cells.last() != Some(&abs) {
            cells.push(abs);
        }
        match par.get(&cur) {
            Some(&p) => cur = p,
            None => break,
        }
    }
    cells.reverse();
    cells
}

/// Path length in cells stepped (a single-cell path is zero steps).
pub fn steps(path: &[(i32, i32)]) -> u64 {
    path.len().saturating_sub(1) as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn straight_runs_are_manhattan() {
        let p = route((0, 0), (10, 0), &HashSet::new());
        assert_eq!(steps(&p), 10);
        let p = route((0, 0), (7, 3), &HashSet::new());
        assert_eq!(steps(&p), 10);
    }

    #[test]
    fn detours_around_a_wall_and_is_deterministic() {
        // A vertical wall between start and goal with one gap far below.
        let mut wall = HashSet::new();
        for y in -5..=2 {
            wall.insert((5, y));
        }
        let p = route((0, 0), (10, 0), &wall);
        assert!(steps(&p) > 10, "must detour: {} steps", steps(&p));
        assert!(!p.iter().any(|c| wall.contains(c)), "never through the wall");
        let q = route((0, 0), (10, 0), &wall);
        assert_eq!(p, q, "same inputs, same route");
    }

    #[test]
    fn routes_are_translation_covariant() {
        let mut wall = HashSet::new();
        for y in -3..=3 {
            wall.insert((4, y));
        }
        let p = route((0, 0), (9, 1), &wall);
        let shifted: HashSet<(i32, i32)> = wall.iter().map(|&(x, y)| (x + 7, y - 13)).collect();
        let q = route((7, -13), (16, -12), &shifted);
        let back: Vec<(i32, i32)> = q.iter().map(|&(x, y)| (x - 7, y + 13)).collect();
        assert_eq!(p, back, "translated world, translated route");
    }

    #[test]
    fn free_start_walks_west_in_a_straight_line() {
        let p = route_free((10, 0), (2, 0), &HashSet::new());
        assert_eq!(steps(&p), 8, "westbound is 8 straight steps: {p:?}");
        assert!(p.iter().all(|&(_, y)| y == 0), "no doorstep spiral: {p:?}");
    }

    #[test]
    fn hemmed_in_falls_back_to_the_l_path() {
        // Box the goal in completely.
        let mut boxed = HashSet::new();
        for x in 7..=11 {
            for y in -2..=2 {
                if (x, y) != (9, 0) {
                    boxed.insert((x, y));
                }
            }
        }
        let p = route((0, 0), (9, 0), &boxed);
        assert_eq!(steps(&p), 9, "L-path length is Manhattan");
    }
}
