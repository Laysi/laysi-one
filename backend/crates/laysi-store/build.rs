// Adding a migration file must force this crate to recompile.
//
// `sqlx::migrate!("../../migrations")` embeds the directory at compile time,
// but cargo only tracks the files the macro happened to read. A brand new
// file therefore leaves a stale binary that starts, reports an older
// migration version and silently serves without the new schema.
fn main() {
    println!("cargo:rerun-if-changed=../../migrations");
    // Same reason for the regulation channel: its files are embedded with
    // include_str!, and a new one that does not force a rebuild ships a
    // binary that reports success while writing no amendment.
    println!("cargo:rerun-if-changed=../../regulation-updates");
}
