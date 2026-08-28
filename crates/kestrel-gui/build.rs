//! Build script: compile the Slint UI definitions (ADR 0001).
fn main() {
    slint_build::compile("ui/app.slint").unwrap_or_else(|e| panic!("slint compile failed: {e}"));
}
