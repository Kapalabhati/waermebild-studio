// Embeds the Windows version information (product name, version) into the .exe.
// SignPath checks these fields before signing.

fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut res = winresource::WindowsResource::new();
        res.set("ProductName", "Waermebild-Studio")
            .set("FileDescription", "Waermebild-Studio")
            .set("CompanyName", "kapalabathi")
            .set("LegalCopyright", "Copyright (c) 2026 kapalabathi, MIT License")
            .set("OriginalFilename", "waermebild-studio.exe");
        res.compile().expect("Windows-Ressourcen konnten nicht eingebettet werden");
    }
}
