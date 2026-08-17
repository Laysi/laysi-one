// Adding a migration file must force this crate to recompile.
//
// `sqlx::migrate!("../../migrations")` embeds the directory at compile time,
// but cargo only tracks the files the macro happened to read. A brand new
// file therefore leaves a stale binary that starts, reports an older
// migration version and silently serves without the new schema.
fn main() {
    println!("cargo:rerun-if-changed=../../migrations");
}
