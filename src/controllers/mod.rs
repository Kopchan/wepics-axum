mod album;
mod media;
mod page;
mod pagination;
mod system;

pub use album::{AlbumInfo, AlbumPage, get_album, list_children};
pub use media::{MediaPage, get_file, list_media};
pub(crate) use page::public_dir;
pub use page::{favicon, root_page, spa};
pub use system::{health, hello, hello_name};
