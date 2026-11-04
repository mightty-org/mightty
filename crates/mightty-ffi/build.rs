//! On macOS, room in the dylib's header for any install name, so
//! `scripts/install-lib.sh` can name where it was installed.

fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        println!("cargo:rustc-cdylib-link-arg=-Wl,-headerpad_max_install_names");
    }
}
