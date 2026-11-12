fn main() {
    // The target triple, for self-update's release-asset selection.
    println!(
        "cargo:rustc-env=BUILD_TARGET={}",
        std::env::var("TARGET").unwrap()
    );
}
