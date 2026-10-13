// `sqlx::migrate!` embeds the migration directories at compile time, and cargo
// does not otherwise notice a file added to them.
fn main() {
    println!("cargo:rerun-if-changed=migrations");
}
