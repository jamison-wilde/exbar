fn main() {
    #[cfg(windows)]
    {
        // Application manifest declaring per-monitor-v2 DPI awareness.
        // Windows parses this BEFORE the process runs any user code, so
        // the toolbar's layout math sees real pixel units from instant
        // zero — no risk of a user32 call (clap, windows-rs init, etc.)
        // locking the DPI context before we can set it at runtime.
        const MANIFEST: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<assembly xmlns="urn:schemas-microsoft-com:asm.v1" manifestVersion="1.0">
  <application xmlns="urn:schemas-microsoft-com:asm.v3">
    <windowsSettings>
      <dpiAwareness xmlns="http://schemas.microsoft.com/SMI/2016/WindowsSettings">PerMonitorV2</dpiAwareness>
      <dpiAware xmlns="http://schemas.microsoft.com/SMI/2005/WindowsSettings">True/PM</dpiAware>
    </windowsSettings>
  </application>
  <compatibility xmlns="urn:schemas-microsoft-com:compatibility.v1">
    <application>
      <!-- Windows 10 / 11 -->
      <supportedOS Id="{8e0f7a12-bfb3-4fe8-b9a5-48fd50a15a9a}"/>
    </application>
  </compatibility>
</assembly>"#;

        let mut res = winres::WindowsResource::new();
        res.set("FileDescription", "Exbar CLI")
            .set("ProductName", "Exbar")
            .set("CompanyName", "Exbar")
            .set("LegalCopyright", "Copyright (c) 2026")
            .set("OriginalFilename", "exbar.exe")
            .set("FileVersion", env!("CARGO_PKG_VERSION"))
            .set("ProductVersion", env!("CARGO_PKG_VERSION"))
            .set_manifest(MANIFEST);
        let _ = res.compile();
    }
}
