//! Structural item types: shapes instead of numbers.
//!
//! The caching economy rests on items having no identity — wires carry
//! counts per type. Structures don't break that: **the structure lives in
//! the type.** An [`ItemType`] is an index into the [`StructLib`], where
//! each type is a canonical 2D cell-set (cells of materials, bounding box
//! anchored at the origin), hash-consed exactly like nets. Every item of a
//! type is interchangeable; the type itself is the artifact.
//!
//! Equality is **extensional**: two factories assembling the same shape by
//! different routes produce literally the same type (same id, same cache
//! entries, same goal credit). Constructors are functions on shapes:
//! [`StructLib::weld`] (union at an offset; *refuses* if cells collide —
//! parts must fit) and [`StructLib::rot`] (quarter-turn). The counting
//! kernel never looks inside a type, so none of this touches evaluation:
//! rates and shapes are orthogonal by construction.

use std::collections::HashMap;

use crate::net::ItemType;

/// Placeholder type on a polymorphic builder port, resolved by wiring at
/// compile time.
pub const ANY: ItemType = ItemType(u32::MAX);

/// A material index (colors live in the presentation layer).
pub type Material = u8;
pub const MATERIALS: [&str; 8] =
    ["iron", "copper", "gear", "plate", "tok", "grant", "demand", "pulse"];

/// One cell: position + material. Structures are sets of these.
pub type Cell = (i32, i32, Material);

/// Maximum cells per structure — a friendly cap, not a soundness one.
pub const MAX_CELLS: usize = 1024;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct Shape {
    /// Sorted, origin-anchored (min corner at (0,0)).
    cells: Vec<Cell>,
}

/// Interning store for structural types. `ItemType(i)` indexes into it.
#[derive(Debug, Default)]
pub struct StructLib {
    shapes: Vec<Shape>,
    names: Vec<Option<String>>,
    index: HashMap<Shape, u32>,
}

fn canonical(mut cells: Vec<Cell>) -> Result<Vec<Cell>, String> {
    if cells.is_empty() {
        return Err("a structure needs at least one cell".into());
    }
    if cells.len() > MAX_CELLS {
        return Err(format!("structure too large (over {MAX_CELLS} cells)"));
    }
    let minx = cells.iter().map(|c| c.0).min().unwrap();
    let miny = cells.iter().map(|c| c.1).min().unwrap();
    for c in &mut cells {
        c.0 -= minx;
        c.1 -= miny;
    }
    cells.sort_unstable();
    for w in cells.windows(2) {
        if w[0].0 == w[1].0 && w[0].1 == w[1].1 {
            return Err("parts collide: two cells at the same position".into());
        }
    }
    Ok(cells)
}

impl StructLib {
    /// A library seeded with the eight single-cell primitives, in material
    /// order — so `ItemType(0..8)` keep their historical meanings.
    pub fn new() -> StructLib {
        let mut lib = StructLib::default();
        for (m, name) in MATERIALS.iter().enumerate() {
            let id = lib
                .intern(vec![(0, 0, m as Material)], Some(name.to_string()))
                .expect("primitives are valid");
            debug_assert_eq!(id.0 as usize, m);
        }
        lib
    }

    fn intern(&mut self, cells: Vec<Cell>, name: Option<String>) -> Result<ItemType, String> {
        let shape = Shape { cells: canonical(cells)? };
        if let Some(&id) = self.index.get(&shape) {
            return Ok(ItemType(id));
        }
        let id = self.shapes.len() as u32;
        self.index.insert(shape.clone(), id);
        self.shapes.push(shape);
        self.names.push(name);
        Ok(ItemType(id))
    }

    /// Intern an arbitrary shape (used for chassis and targets).
    pub fn shape(&mut self, cells: Vec<Cell>, name: &str) -> Result<ItemType, String> {
        self.intern(cells, Some(name.to_string()))
    }

    pub fn contains(&self, ty: ItemType) -> bool {
        (ty.0 as usize) < self.shapes.len()
    }

    pub fn cells(&self, ty: ItemType) -> &[Cell] {
        &self.shapes[ty.0 as usize].cells
    }

    pub fn name(&self, ty: ItemType) -> String {
        match self.names[ty.0 as usize].as_deref() {
            Some(n) => n.to_string(),
            None => format!("{}-cell structure #{}", self.cells(ty).len(), ty.0),
        }
    }

    pub fn len(&self) -> usize {
        self.shapes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.shapes.is_empty()
    }

    /// Weld: union of `a` with `b` offset by `(dx, dy)` (in `a`'s frame).
    /// Refuses if any cells land on the same position — parts must fit.
    pub fn weld(&mut self, a: ItemType, b: ItemType, dx: i32, dy: i32) -> Result<ItemType, String> {
        let mut cells = self.cells(a).to_vec();
        cells.extend(self.cells(b).iter().map(|&(x, y, m)| (x + dx, y + dy, m)));
        self.intern(cells, None).map_err(|e| {
            format!("welding {} to {} at ({dx},{dy}): {e}", self.name(a), self.name(b))
        })
    }

    /// Quarter-turn counterclockwise.
    pub fn rot(&mut self, a: ItemType) -> Result<ItemType, String> {
        let cells = self.cells(a).iter().map(|&(x, y, m)| (-y, x, m)).collect();
        self.intern(cells, None)
    }
}

/// Machine-types are structures too: every machine kind has a chassis —
/// a real, interned shape. The welder's chassis doubles as the game's
/// first manufacturing target: build the machine that builds.
pub fn chassis(structs: &mut StructLib, kind: &str) -> ItemType {
    const FE: Material = 0; // iron
    const CU: Material = 1; // copper
    const GR: Material = 2; // gear
    const PL: Material = 3; // plate
    let cells: Vec<Cell> = match kind {
        // The L: an iron column with a copper-tipped arm.
        "weld" => vec![(0, 0, FE), (0, 1, CU), (0, 2, FE), (1, 2, CU)],
        "rot" => vec![(0, 0, GR), (1, 0, PL), (1, 1, GR), (0, 1, PL)],
        "split" => vec![(0, 1, PL), (1, 1, FE), (2, 1, PL), (1, 0, FE)],
        "belt" => vec![(0, 0, PL), (1, 0, PL)],
        "recipe" => vec![(0, 0, FE), (1, 0, GR), (0, 1, FE), (1, 1, FE)],
        "priority" => vec![(1, 0, CU), (0, 1, CU), (1, 1, GR), (2, 1, CU), (1, 2, CU)],
        "module" => vec![
            (0, 0, PL), (1, 0, PL), (2, 0, PL), (0, 1, PL), (2, 1, PL),
            (0, 2, PL), (1, 2, PL), (2, 2, PL),
        ],
        _ => vec![(0, 0, PL)],
    };
    structs
        .shape(cells, &format!("{kind} chassis"))
        .expect("chassis art is valid")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn primitives_keep_their_historic_ids() {
        let lib = StructLib::new();
        assert_eq!(lib.len(), 8);
        assert_eq!(lib.name(ItemType(0)), "iron");
        assert_eq!(lib.name(ItemType(7)), "pulse");
        assert_eq!(lib.cells(ItemType(2)), &[(0, 0, 2)]);
    }

    #[test]
    fn welding_is_extensional() {
        let mut lib = StructLib::new();
        let iron = ItemType(0);
        let copper = ItemType(1);
        // Same shape, two different construction orders: same id.
        let ab = lib.weld(iron, copper, 1, 0).unwrap();
        let ba = lib.weld(copper, iron, -1, 0).unwrap();
        assert_eq!(ab, ba, "shape equality, not construction-path equality");
        assert_eq!(lib.cells(ab), &[(0, 0, 0), (1, 0, 1)]);
    }

    #[test]
    fn collisions_refuse_with_a_friendly_message() {
        let mut lib = StructLib::new();
        let iron = ItemType(0);
        let err = lib.weld(iron, iron, 0, 0).unwrap_err();
        assert!(err.contains("collide"), "{err}");
        assert!(err.contains("iron"), "names the parts: {err}");
    }

    #[test]
    fn rotation_is_a_quarter_turn_with_period_four() {
        let mut lib = StructLib::new();
        let iron = ItemType(0);
        let copper = ItemType(1);
        let bar = lib.weld(iron, copper, 1, 0).unwrap(); // horizontal
        let up = lib.rot(bar).unwrap(); // vertical
        assert_ne!(bar, up);
        assert_eq!(lib.cells(up), &[(0, 0, 0), (0, 1, 1)]);
        let r2 = lib.rot(up).unwrap();
        let r3 = lib.rot(r2).unwrap();
        let r4 = lib.rot(r3).unwrap();
        assert_eq!(r4, bar, "four quarter-turns come home");
    }

    #[test]
    fn welded_structures_compose_into_bigger_ones() {
        let mut lib = StructLib::new();
        let iron = ItemType(0);
        let copper = ItemType(1);
        let bar = lib.weld(iron, copper, 1, 0).unwrap();
        let vbar = lib.rot(bar).unwrap();
        // L-shape: vertical bar + horizontal bar on top.
        let ell = lib.weld(vbar, bar, 0, 2).unwrap();
        assert_eq!(lib.cells(ell).len(), 4);
        // Names degrade gracefully for constructed shapes.
        assert!(lib.name(ell).contains("4-cell"));
    }
}
