//! Small, framework-free building blocks shared by handlers.

pub mod flags;
pub mod pagination;
pub mod thumb;
pub mod vips;

pub use pagination::{MAX_LIMIT, Pagination};
