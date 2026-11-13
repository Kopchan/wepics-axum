mod album;
mod page;
mod pagination;
mod system;
pub mod v1;

pub(crate) use page::public_dir;
pub use page::{root_page, spa};
pub use system::health;
