mod album;
mod index;
mod media;
mod page;
mod pagination;
mod system;

pub use album::{AlbumInfo, AlbumPage, get_album, list_children};
pub use index::{index_events, start_index};
pub use media::{MediaPage, get_file, list_media};
pub(crate) use page::public_dir;
pub use page::{root_page, spa};
pub use system::health;
