use std::env;

fn main() {
    // Snapshot driver support is determined in build.rs and enabled with #[cfg] attributes,
    // so that all drivers (including the always-available "none" driver) are defined consistently
    // throughout the main snapshot code.
    println!(r#"cargo::rerun-if-changed=build.rs"#);
    println!(r#"cargo::rustc-check-cfg=cfg(boi_has_driver, values("none", "apfs"))"#);
    println!(r#"cargo::rustc-cfg=boi_has_driver="none""#);
    if env::var("CARGO_CFG_TARGET_VENDOR").is_ok_and(|v| v == "apple") {
        println!(r#"cargo::rustc-cfg=boi_has_driver="apfs""#);
    }
}
