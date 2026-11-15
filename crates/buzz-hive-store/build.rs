fn main() {
    // The embedded migrations come from `migrations/`; rebuild when one changes.
    println!("cargo:rerun-if-changed=migrations");
}
