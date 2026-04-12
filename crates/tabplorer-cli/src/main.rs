use clap::{Parser, Subcommand};
use std::ffi::OsStr;
use std::os::windows::ffi::OsStrExt;
use std::path::PathBuf;
use std::process::Command;

use windows::core::PCWSTR;
use windows::Win32::System::Registry::{
    RegCloseKey, RegCreateKeyW, RegDeleteTreeW, RegSetValueExW,
    HKEY, HKEY_CURRENT_USER, REG_SZ,
};
use windows::Win32::Foundation::{WIN32_ERROR, HINSTANCE, FreeLibrary};
use windows::Win32::System::LibraryLoader::{LoadLibraryW, GetProcAddress};
use windows::Win32::UI::WindowsAndMessaging::{
    SetWindowsHookExW, GetMessageW, WH_CBT, MSG, HOOKPROC,
};

// ── CLSID (must match tabplorer-dll) ─────────────────────────────────────────

const CLSID: &str = "{7D2B5E4A-89C1-4F3E-A6D8-1B9E0C5F2A73}";

// ── CLI definition ────────────────────────────────────────────────────────────

#[derive(Parser)]
#[command(name = "tabplorer", about = "Manage the Tabplorer Explorer extension")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Install the Explorer extension
    Install,
    /// Uninstall the Explorer extension
    Uninstall {
        /// Also delete the DLL and local data
        #[arg(long)]
        clean: bool,
    },
    /// Show installation status
    Status,
    /// Run as background hook process (used internally by install)
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

fn reg_delete_tree(subkey: &str) -> WinResult<()> {
    let subkey_w = to_wide_null(subkey);
    let err: WIN32_ERROR = unsafe {
        RegDeleteTreeW(HKEY_CURRENT_USER, PCWSTR(subkey_w.as_ptr()))
    };
    // ERROR_FILE_NOT_FOUND (2) means already absent — that's fine
    if err.is_ok() || err.0 == 2 { Ok(()) } else { Err(win_err(err)) }
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
    local_appdata().join("tabplorer")
}

fn install_dll_path() -> PathBuf {
    install_dir().join("tabplorer_dll.dll")
}

fn source_dll_path() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let dll = exe.parent()?.join("tabplorer_dll.dll");
    if dll.exists() { Some(dll) } else { None }
}

fn config_path() -> PathBuf {
    let home = std::env::var("USERPROFILE")
        .or_else(|_| std::env::var("HOME"))
        .unwrap_or_else(|_| "C:\\Users\\Default".into());
    PathBuf::from(home).join(".tabplorer.json")
}

// ── Explorer restart ──────────────────────────────────────────────────────────

fn restart_explorer() {
    let _ = Command::new("taskkill")
        .args(["/f", "/im", "explorer.exe"])
        .output();
    let _ = Command::new("cmd")
        .args(["/c", "start", "explorer.exe"])
        .output();
    println!("Explorer restarted.");
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
const RUN_VALUE: &str = "Tabplorer";

// ── hook ──────────────────────────────────────────────────────────────────────

/// Load tabplorer_dll.dll and install a global CBT hook, then run a message
/// loop to keep it alive.  This function never returns normally.
fn run_hook() -> WinResult<()> {
    let dll_path = install_dll_path();
    let dll_path_wide = to_wide_null(&dll_path.to_string_lossy());

    // Load the DLL
    let hmod = unsafe {
        LoadLibraryW(PCWSTR(dll_path_wide.as_ptr()))?
    };

    // Get the hook proc address
    let proc_name = std::ffi::CString::new("TabplorerCBTHook").unwrap();
    let hook_fn = unsafe {
        GetProcAddress(hmod, windows::core::PCSTR(proc_name.as_ptr().cast()))
    };
    let hook_fn = hook_fn.ok_or_else(|| {
        windows_core::Error::new(
            windows_core::HRESULT(0x80004005u32 as i32),
            "TabplorerCBTHook export not found in tabplorer_dll.dll",
        )
    })?;

    // Transmute to HOOKPROC — safe because we verified the export exists
    let hook_proc: HOOKPROC = unsafe { std::mem::transmute(hook_fn) };

    // Convert HMODULE to HINSTANCE (same underlying pointer)
    let hinstance = HINSTANCE(hmod.0);

    // Install the global CBT hook (thread_id = 0 → all threads)
    let _hhook = unsafe {
        SetWindowsHookExW(WH_CBT, hook_proc, Some(hinstance), 0)?
    };

    println!("Tabplorer hook running. Press Ctrl+C to stop.");

    // Run the message loop to keep the hook alive
    let mut msg = MSG::default();
    loop {
        let ret = unsafe { GetMessageW(&mut msg, None, 0, 0) };
        if ret.0 == 0 || ret.0 == -1 {
            break;
        }
    }

    // Cleanup (unreachable in normal operation; process is killed externally)
    unsafe { let _ = FreeLibrary(hmod); }
    Ok(())
}

// ── install ───────────────────────────────────────────────────────────────────

fn install() -> WinResult<()> {
    // 1. Find source DLL
    let src = source_dll_path().ok_or_else(|| {
        windows_core::Error::new(
            windows_core::HRESULT(0x80070002u32 as i32),
            "tabplorer_dll.dll not found next to the executable",
        )
    })?;

    // 2. Copy to %LOCALAPPDATA%\tabplorer\
    let dst_dir = install_dir();
    std::fs::create_dir_all(&dst_dir).map_err(io_err)?;
    let dst = install_dll_path();
    std::fs::copy(&src, &dst).map_err(io_err)?;
    println!("Copied DLL to {}", dst.display());

    let dll_path = dst.to_string_lossy().into_owned();

    // 3. Register COM CLSID InprocServer32
    let clsid_key = format!(r"Software\Classes\CLSID\{CLSID}\InprocServer32");
    let hkey = reg_create_key(&clsid_key)?;
    reg_set_string(hkey, "", &dll_path)?;          // default value = DLL path
    reg_set_string(hkey, "ThreadingModel", "Apartment")?;
    unsafe { let _ = RegCloseKey(hkey); };
    println!("Registered CLSID InprocServer32.");

    // 4. Register BHO
    let bho_key = format!(
        r"Software\Microsoft\Windows\CurrentVersion\Explorer\Browser Helper Objects\{CLSID}"
    );
    let hkey = reg_create_key(&bho_key)?;
    reg_set_string(hkey, "NoExplorer", "0")?;
    unsafe { let _ = RegCloseKey(hkey); };
    println!("Registered BHO.");

    // 5. Create stub config if missing
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

    // 6. Register Run key so hook starts at logon
    let exe_path = std::env::current_exe()
        .map_err(io_err)?
        .to_string_lossy()
        .into_owned();
    let run_value = format!("\"{exe_path}\" hook");
    let hkey = reg_create_key(RUN_KEY)?;
    reg_set_string(hkey, RUN_VALUE, &run_value)?;
    unsafe { let _ = RegCloseKey(hkey); }
    println!("Registered Run key: {run_value}");

    // 7. Restart Explorer first (so the old session is gone)
    restart_explorer();

    // 8. Start hook process after Explorer restarts (detached)
    std::thread::sleep(std::time::Duration::from_secs(2));
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
    // 1. Remove BHO registry key
    let bho_key = format!(
        r"Software\Microsoft\Windows\CurrentVersion\Explorer\Browser Helper Objects\{CLSID}"
    );
    reg_delete_tree(&bho_key)?;
    println!("Removed BHO registry key.");

    // 2. Remove CLSID registry key
    let clsid_key = format!(r"Software\Classes\CLSID\{CLSID}");
    reg_delete_tree(&clsid_key)?;
    println!("Removed CLSID registry key.");

    // 3. Remove Run key
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

    // 4. Kill any running hook process
    let _ = Command::new("taskkill")
        .args(["/f", "/im", "tabplorer.exe"])
        .output();
    println!("Killed hook process (if running).");

    // 5. If --clean, remove install dir
    if clean {
        let dir = install_dir();
        if dir.exists() {
            std::fs::remove_dir_all(&dir).map_err(io_err)?;
            println!("Deleted {}.", dir.display());
        }
    }

    // 6. Restart Explorer
    restart_explorer();
    println!("Uninstall complete. (~/.tabplorer.json left in place)");
    Ok(())
}

// ── status ────────────────────────────────────────────────────────────────────

fn status() -> WinResult<()> {
    let dll = install_dll_path();
    let dll_ok = dll.exists();
    println!("DLL installed:  {} ({})", if dll_ok { "YES" } else { "NO" }, dll.display());

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
