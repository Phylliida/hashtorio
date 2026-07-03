//! hashtorio kernel: the semantics of wires is cumulative counting functions
//! (monotone staircases `tick -> count`), and every kernel-fragment behavior
//! is *ultimately periodic*, so it has a finite canonical representation that
//! doubles as a cache key.
//!
//! See DESIGN.md for the full design. This crate is M0 of the roadmap: the
//! [`counting::Counting`] type and its op algebra, plus [`recipe::Recipe`]
//! application for feedforward use.

pub mod counting;
pub mod recipe;

pub use counting::Counting;
pub use recipe::Recipe;
