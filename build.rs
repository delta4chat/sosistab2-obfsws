pub fn main() {
    // NOTE: This will output everything, and requires all features enabled.
    // NOTE: See the EmitBuilder documentation for configuration options.
    vergen::EmitBuilder::builder()
        .all_build()
        .all_cargo()
        .all_git()
        .all_rustc()
        .emit().unwrap();

}
