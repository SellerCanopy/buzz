fn main() {
    // `sqlx::migrate!` embeds the files in `migrations/`; rebuild when one changes.
    println!("cargo:rerun-if-changed=migrations");
}
