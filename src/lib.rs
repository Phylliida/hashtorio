//! hashtorio kernel: the semantics of wires is cumulative counting functions
//! (monotone staircases `tick -> count`), and every kernel-fragment behavior
//! is *ultimately periodic*, so it has a finite canonical representation that
//! doubles as a cache key.
//!
//! See DESIGN.md for the full design. Layers:
//! - [`counting`]: the summary data structure and its op algebra (M0)
//! - [`recipe`]: the kernel's one work primitive (M0)
//! - [`net`]: typed wiring terms and the hash-consed blueprint library (M1)
//! - [`flatten`]: module inlining (M1)

pub mod counting;
pub mod flatten;
pub mod net;
pub mod recipe;

pub use counting::Counting;
pub use net::{ItemType, Library, Net, NetBuilder, NetId};
pub use recipe::Recipe;
