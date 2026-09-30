//! Embeds the icon and version metadata into the .exe on Windows (Explorer
//! icon, Details tab of the file properties). Does nothing elsewhere.

fn main() {
    println!("cargo:rerun-if-changed=assets/icon.ico");
    println!("cargo:rerun-if-changed=build.rs");
    // Version and authors come from Cargo.toml. Once any rerun-if-changed is
    // declared, Cargo only reruns for the listed files.
    println!("cargo:rerun-if-changed=Cargo.toml");

    #[cfg(windows)]
    {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("assets/icon.ico");
        res.set("ProductName", "Photo Sorter");
        res.set("FileDescription", "Photo Sorter");
        res.set("LegalCopyright", "MIT licensed");

        // Version fields are filled in from Cargo.toml automatically.
        if let Ok(authors) = std::env::var("CARGO_PKG_AUTHORS") {
            if !authors.is_empty() {
                res.set("CompanyName", &authors);
            }
        }

        if let Err(e) = res.compile() {
            // An .exe without an icon still works: warn, don't fail.
            println!("cargo:warning=could not embed Windows resources: {e}");
        }
    }
}
