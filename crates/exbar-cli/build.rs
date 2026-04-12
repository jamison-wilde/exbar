fn main() {
    #[cfg(windows)]
    {
        let mut res = winres::WindowsResource::new();
        res.set("FileDescription", "Exbar CLI")
            .set("ProductName", "Exbar")
            .set("CompanyName", "Exbar")
            .set("LegalCopyright", "Copyright (c) 2026")
            .set("OriginalFilename", "exbar.exe")
            .set("FileVersion", env!("CARGO_PKG_VERSION"))
            .set("ProductVersion", env!("CARGO_PKG_VERSION"));
        let _ = res.compile();
    }
}
