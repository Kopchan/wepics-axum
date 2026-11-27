//! Extra link metadata for the `vips` crate on MSVC with a manual install.
//!
//! `vips-sys` links `vips.lib` and, on MSVC, relies on vcpkg to also pull in the
//! GLib/GObject import libraries that the safe wrapper calls (`g_free`,
//! `g_object_unref`, `g_signal_connect_data`). Providing `LIBVIPS_LIB_DIR`
//! (as done for a scoop/native libvips install) bypasses vcpkg, so the GLib
//! import libraries next to `vips.lib` have to be named here.

use std::{env, fs, path::Path};

fn main() {
    embed_windows_icon();
    link_vips_msvc();
}

fn link_vips_msvc() {
    println!("cargo:rerun-if-env-changed=LIBVIPS_LIB_DIR");

    if env::var("CARGO_CFG_TARGET_ENV").as_deref() != Ok("msvc") {
        return;
    }
    let Ok(lib_dir) = env::var("LIBVIPS_LIB_DIR") else {
        // vcpkg (or another discovery path) supplies the metadata itself.
        return;
    };

    println!("cargo:rustc-link-search=native={lib_dir}");
    for lib in ["libgobject-2.0", "libgio-2.0", "libglib-2.0"] {
        println!("cargo:rustc-link-lib=dylib={lib}");
    }
}

#[cfg(not(target_os = "windows"))]
fn embed_windows_icon() {}

#[cfg(target_os = "windows")]
fn embed_windows_icon() {
    println!("cargo:rerun-if-changed=public/assets/icon");

    let ico_path = Path::new("public/assets/icon/app.ico");
    let mut icon_dir = ico::IconDir::new(ico::ResourceType::Icon);
    let sizes = [24, 32, 48, 64, 96, 128, 192, 256];

    for size in sizes {
        let png_path = format!("public/assets/icon/app{}.png", size);
        if Path::new(&png_path).exists()
            && let Ok(img) = image::open(&png_path)
        {
            let rgba = img.into_rgba8();
            let icon_image = ico::IconImage::from_rgba_data(size, size, rgba.into_raw());
            icon_dir.add_entry(ico::IconDirEntry::encode(&icon_image).unwrap());
        }
    }

    if let Ok(file) = fs::File::create(ico_path) {
        let _ = icon_dir.write(file);
    }

    if env::var("CARGO_CFG_TARGET_OS").unwrap_or_default() == "windows" {
        let mut res = winresource::WindowsResource::new();
        res.set_icon(ico_path.to_str().unwrap());
        res.compile().expect("Failed embed icon into .exe");
    }
}
