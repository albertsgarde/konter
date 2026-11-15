//! Puts the commit konter was built from in `konter --version`. The Nix flake
//! sets KONTER_REV; a plain `cargo build` says "dev".

fn main() {
    println!("cargo::rerun-if-env-changed=KONTER_REV");
    let rev = std::env::var("KONTER_REV").unwrap_or_else(|_| "dev".into());
    println!("cargo::rustc-env=KONTER_VERSION={} ({rev})", env!("CARGO_PKG_VERSION"));
}
