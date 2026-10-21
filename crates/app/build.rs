//! Rebuilds when a migration is added, so every `sqlx::migrate!` embeds every file in its
//! directory. A procedural macro sees changes to the files it reads, but not new files.

fn main() {
    println!("cargo::rerun-if-changed=../../migrations");
    println!("cargo::rerun-if-changed=tests/integration/migrations");
}
