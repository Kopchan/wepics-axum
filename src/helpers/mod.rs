//! Small, framework-free building blocks shared by handlers.

pub mod flags;
pub mod hashes;
pub mod pagination;
pub mod secure_link;
pub mod thumb;
pub mod video;

pub use pagination::{MAX_LIMIT, Pagination};
