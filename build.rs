//! Extra link metadata for the `vips` crate on MSVC with a manual install.
//!
//! `vips-sys` links `vips.lib` and, on MSVC, relies on vcpkg to also pull in the
//! GLib/GObject import libraries that the safe wrapper calls (`g_free`,
//! `g_object_unref`, `g_signal_connect_data`). Providing `LIBVIPS_LIB_DIR`
//! (as done for a scoop/native libvips install) bypasses vcpkg, so the GLib
//! import libraries next to `vips.lib` have to be named here.

fn main() {
    println!("cargo:rerun-if-env-changed=LIBVIPS_LIB_DIR");

    if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() != Ok("msvc") {
        return;
    }
    let Ok(lib_dir) = std::env::var("LIBVIPS_LIB_DIR") else {
        // vcpkg (or another discovery path) supplies the metadata itself.
        return;
    };

    println!("cargo:rustc-link-search=native={lib_dir}");
    for lib in ["libgobject-2.0", "libgio-2.0", "libglib-2.0"] {
        println!("cargo:rustc-link-lib=dylib={lib}");
    }
}
