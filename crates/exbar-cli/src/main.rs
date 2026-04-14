// In release builds, mark this binary as Windows-subsystem so no console
// is allocated at launch. A console-subsystem binary, when launched via
// the MSI post-install action, the Run key, or a Start Menu shortcut,
// causes Windows Terminal (if set as default terminal) to briefly claim
// the console as a new tab — which then vanishes when `FreeConsole()`
// runs inside `run_hook()`. Debug builds remain console-subsystem so
// `cargo run -- status` prints to the terminal during development.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use clap::{Parser, Subcommand};
use std::ffi::OsStr;
use std::os::windows::ffi::OsStrExt;
use std::path::PathBuf;
use std::process::Command;

use windows::core::PCWSTR;
use windows::Win32::System::Registry::{
    RegCloseKey, RegCreateKeyW, RegSetValueExW,
    HKEY, HKEY_CURRENT_USER, REG_SZ,
};
use windows::Win32::Foundation::WIN32_ERROR;

mod log;
mod theme;
mod config;
mod explorer;
mod shell_windows;
mod navigate;
mod picker;
mod contextmenu;
mod dragdrop;
mod toolbar;

// ── CLI definition ────────────────────────────────────────────────────────────

#[derive(Parser)]
#[command(name = "exbar", about = "Manage the Exbar Explorer toolbar")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// [DEV ONLY] Install the Explorer extension (end users should use the MSI installer)
    Install,
    /// [DEV ONLY] Uninstall the Explorer extension (end users should use Windows Settings → Apps)
    Uninstall {
        /// Also delete the DLL and local data
        #[arg(long)]
        clean: bool,
    },
    /// Show installation status
    Status,
    /// Run as background hook process (started by the MSI installer's Run key)
    Hook,
}

fn main() {
    let cli = Cli::parse();
    match cli.command {
        Commands::Install => {
            if let Err(e) = install() {
                eprintln!("Install failed: {e}");
                std::process::exit(1);
            }
        }
        Commands::Uninstall { clean } => {
            if let Err(e) = uninstall(clean) {
                eprintln!("Uninstall failed: {e}");
                std::process::exit(1);
            }
        }
        Commands::Status => {
            if let Err(e) = status() {
                eprintln!("Status failed: {e}");
                std::process::exit(1);
            }
        }
        Commands::Hook => {
            if let Err(e) = run_hook() {
                eprintln!("Hook failed: {e}");
                std::process::exit(1);
            }
        }
    }
}

// ── Helper: wide string ───────────────────────────────────────────────────────

fn to_wide_null(s: &str) -> Vec<u16> {
    OsStr::new(s)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

// ── Helper: registry ──────────────────────────────────────────────────────────

type WinResult<T> = Result<T, windows_core::Error>;

fn win_err(err: WIN32_ERROR) -> windows_core::Error {
    windows_core::Error::from(err.to_hresult())
}

/// Open or create a key under HKCU.
fn reg_create_key(subkey: &str) -> WinResult<HKEY> {
    let subkey_w = to_wide_null(subkey);
    let mut hkey = HKEY::default();
    let err: WIN32_ERROR = unsafe {
        RegCreateKeyW(
            HKEY_CURRENT_USER,
            PCWSTR(subkey_w.as_ptr()),
            &mut hkey,
        )
    };
    if err.is_ok() {
        Ok(hkey)
    } else {
        Err(win_err(err))
    }
}

fn reg_set_string(hkey: HKEY, value_name: &str, data: &str) -> WinResult<()> {
    let name_w = to_wide_null(value_name);
    let data_w = to_wide_null(data);
    let data_bytes: &[u8] = unsafe {
        std::slice::from_raw_parts(
            data_w.as_ptr().cast::<u8>(),
            data_w.len() * 2,
        )
    };
    let err: WIN32_ERROR = unsafe {
        RegSetValueExW(
            hkey,
            PCWSTR(name_w.as_ptr()),
            None,
            REG_SZ,
            Some(data_bytes),
        )
    };
    if err.is_ok() { Ok(()) } else { Err(win_err(err)) }
}

// ── Install paths ─────────────────────────────────────────────────────────────

fn local_appdata() -> PathBuf {
    PathBuf::from(
        std::env::var("LOCALAPPDATA").unwrap_or_else(|_| {
            format!(
                r"C:\Users\{}\AppData\Local",
                std::env::var("USERNAME").unwrap_or_default()
            )
        }),
    )
}

fn install_dir() -> PathBuf {
    local_appdata().join("Exbar")
}

fn config_path() -> PathBuf {
    let home = std::env::var("USERPROFILE")
        .or_else(|_| std::env::var("HOME"))
        .unwrap_or_else(|_| "C:\\Users\\Default".into());
    PathBuf::from(home).join(".exbar.json")
}

// ── io err helper ─────────────────────────────────────────────────────────────

fn io_err(e: std::io::Error) -> windows_core::Error {
    windows_core::Error::new(
        windows_core::HRESULT(0x80004005u32 as i32), // E_FAIL
        e.to_string().as_str(),
    )
}

// ── Run key path ──────────────────────────────────────────────────────────────

const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const RUN_VALUE: &str = "Exbar";

// ── hook ──────────────────────────────────────────────────────────────────────

fn run_hook() -> WinResult<()> {
    use windows::Win32::System::Com::{
        CoInitializeEx, CoUninitialize, COINIT_APARTMENTTHREADED,
    };
    use windows::Win32::System::Console::FreeConsole;
    use windows::Win32::UI::WindowsAndMessaging::{
        DispatchMessageW, GetMessageW, TranslateMessage, MSG,
    };

    // Detach from any inherited console so no terminal window appears.
    unsafe { let _ = FreeConsole(); }

    // STA for COM — IShellWindows, IFileOperation, drag-drop, folder picker.
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
    }

    // Install the foreground event hook. Because WINEVENT_OUTOFCONTEXT
    // marshals callbacks to the thread that installed the hook (provided
    // that thread has a message pump), our GetMessage loop below will
    // drive the toolbar's wndproc AND receive foreground-change events
    // — both on the same thread.
    //
    // The first CabinetWClass foreground event triggers toolbar creation
    // inside foreground_event_proc.
    crate::toolbar::install_foreground_hook();
    crate::log::info("run_hook: foreground hook installed; entering message pump");

    // Message pump — runs indefinitely.
    let mut msg = MSG::default();
    loop {
        let ret = unsafe { GetMessageW(&mut msg, None, 0, 0) };
        if ret.0 == 0 || ret.0 == -1 {
            break;
        }
        unsafe {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }

    // Cleanup — unreachable in normal operation.
    unsafe { CoUninitialize(); }
    Ok(())
}

// ── install ───────────────────────────────────────────────────────────────────

fn install() -> WinResult<()> {
    // Create install directory (for the config stub; the MSI handles
    // the real install path for end users).
    let dst_dir = install_dir();
    std::fs::create_dir_all(&dst_dir).map_err(io_err)?;

    // Create stub config if missing
    let cfg = config_path();
    if !cfg.exists() {
        let stub = serde_json::json!({
            "folders": [
                { "name": "Downloads", "path": "shell:Downloads" },
                { "name": "Documents", "path": "shell:Personal" },
                { "name": "Desktop",   "path": "shell:Desktop" }
            ]
        });
        std::fs::write(&cfg, serde_json::to_string_pretty(&stub).unwrap())
            .map_err(io_err)?;
        println!("Created config at {}", cfg.display());
    } else {
        println!("Config already exists at {}", cfg.display());
    }

    // Register Run key
    let exe_path = std::env::current_exe()
        .map_err(io_err)?
        .to_string_lossy()
        .into_owned();
    let run_value = format!("\"{exe_path}\" hook");
    let hkey = reg_create_key(RUN_KEY)?;
    reg_set_string(hkey, RUN_VALUE, &run_value)?;
    unsafe { let _ = RegCloseKey(hkey); }
    println!("Registered Run key: {run_value}");

    // Start hook process (detached)
    let _ = Command::new(&exe_path)
        .arg("hook")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .stdin(std::process::Stdio::null())
        .spawn()
        .map_err(|_| ())
        .map(|_| println!("Hook process started."));

    println!("Install complete.");
    Ok(())
}

// ── uninstall ─────────────────────────────────────────────────────────────────

fn uninstall(clean: bool) -> WinResult<()> {
    // 1. Remove Run key
    use windows::Win32::System::Registry::{RegOpenKeyW, RegDeleteValueW, HKEY_CURRENT_USER};
    {
        let run_key_w = to_wide_null(RUN_KEY);
        let mut hkey = HKEY::default();
        let err = unsafe { RegOpenKeyW(HKEY_CURRENT_USER, PCWSTR(run_key_w.as_ptr()), &mut hkey) };
        if err.is_ok() {
            let val_w = to_wide_null(RUN_VALUE);
            unsafe { let _ = RegDeleteValueW(hkey, PCWSTR(val_w.as_ptr())); }
            unsafe { let _ = RegCloseKey(hkey); }
            println!("Removed Run key.");
        }
    }

    // 2. Kill any running hook process
    let _ = Command::new("taskkill")
        .args(["/f", "/im", "exbar.exe"])
        .output();
    println!("Killed hook process (if running).");

    // 3. If --clean, remove install dir
    if clean {
        let dir = install_dir();
        if dir.exists() {
            std::fs::remove_dir_all(&dir).map_err(io_err)?;
            println!("Deleted {}.", dir.display());
        }
    }

    println!("Uninstall complete. (~/.exbar.json left in place)");
    Ok(())
}

// ── status ────────────────────────────────────────────────────────────────────

fn status() -> WinResult<()> {
    let exe = std::env::current_exe().map_err(io_err)?;
    println!("exbar.exe:      {}", exe.display());

    let cfg = config_path();
    if cfg.exists() {
        match std::fs::read_to_string(&cfg)
            .ok()
            .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
        {
            Some(v) => {
                let count = v
                    .get("folders")
                    .and_then(|f| f.as_array())
                    .map(|a| a.len())
                    .unwrap_or(0);
                println!("Config:         OK — {count} folder(s) ({})", cfg.display());
            }
            None => {
                println!("Config:         INVALID ({})", cfg.display());
            }
        }
    } else {
        println!("Config:         MISSING ({})", cfg.display());
    }

    Ok(())
}
