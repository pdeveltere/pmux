//! A build id, so a window can tell a background pmux of another build (`hold::BUILD`) and have it
//! hand its terminals over to a new one. Cargo reruns this whenever a file of the package changes,
//! so every build that changes the binary gets a new id.
fn main() {
    let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos());
    println!("cargo:rustc-env=PMUX_BUILD={t:x}");
}
