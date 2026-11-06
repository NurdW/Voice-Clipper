// SPDX-License-Identifier: GPL-3.0-or-later
fn main() {
    // libvosk comes from the Fedora package `vosk-api-devel`, which installs
    // /usr/lib64/libvosk.so. Linking dynamically keeps us off the vendor blob.
    println!("cargo:rustc-link-lib=dylib=vosk");
    println!("cargo:rerun-if-changed=build.rs");
}
