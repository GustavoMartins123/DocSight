fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("cargo:rerun-if-env-changed=TARGET");
    let target = std::env::var("TARGET")?;
    println!("cargo:rustc-env=DOCSIGHT_BUILD_TARGET={target}");
    Ok(())
}
