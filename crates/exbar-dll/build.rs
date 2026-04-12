fn main() {
    #[cfg(windows)]
    {
        let mut res = winres::WindowsResource::new();
        res.set("FileDescription", "Exbar Explorer toolbar")
            .set("ProductName", "Exbar")
            .set("CompanyName", "Exbar")
            .set("LegalCopyright", "Copyright (c) 2026")
            .set("OriginalFilename", "exbar_dll.dll")
            .set("FileVersion", env!("CARGO_PKG_VERSION"))
            .set("ProductVersion", env!("CARGO_PKG_VERSION"));
        // Don't fail the build if resource embedding fails (e.g., rc.exe not found)
        let _ = res.compile();
    }
}
