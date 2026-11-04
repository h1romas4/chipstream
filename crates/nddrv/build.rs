fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("cargo:rerun-if-changed=assets/icon/nddrv-icon.ico");
    if std::env::var("CARGO_CFG_TARGET_OS")? == "windows" {
        if std::env::var("CARGO_CFG_TARGET_ENV")? == "msvc" {
            println!("cargo:rustc-link-arg-bin=nddrv=/SUBSYSTEM:WINDOWS");
            println!("cargo:rustc-link-arg-bin=nddrv=/ENTRY:mainCRTStartup");
        }
        winresource::WindowsResource::new()
            .set_icon("assets/icon/nddrv-icon.ico")
            .compile()?;
    }
    Ok(())
}
