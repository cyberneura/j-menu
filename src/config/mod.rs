//! Configuration discovery, parsing and merging.

pub mod discovery;
pub mod loader;
pub mod model;

pub use loader::{Config, load};
pub use model::{ArgSpec, Job, Launch, MenuItem, ParallelMode};
