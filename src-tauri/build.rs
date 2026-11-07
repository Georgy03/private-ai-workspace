fn main() {
    // The app's icon is built into the program. Without this, changing an icon file does not make
    // Cargo rebuild, and the old icon keeps showing.
    println!("cargo:rerun-if-changed=icons");
    println!("cargo:rerun-if-changed=tauri.conf.json");
    tauri_build::build()
}
