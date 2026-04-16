//! UIA spike — attach to a live file dialog, dump its UIA tree, test selector candidates.
//!
//! Usage: bring a file dialog to foreground, then run:
//!   cargo run -p exbar-cli --bin uia_spike -- "C:\\Users\\slain\\Downloads"
//!
//! You have 3 seconds after invocation to bring the target dialog to foreground.
//! The binary dumps every Edit descendant (AutomationId, Name) found in the dialog's
//! UIA tree, then walks through each and attempts: ValuePattern.SetValue(path) + Enter.
//! Between attempts, press Enter in the terminal to continue to the next candidate.
//!
//! Observe the dialog after each attempt: whichever Edit causes the dialog to navigate
//! to the target path is our selector.

use std::env;
use std::time::Duration;
use windows::Win32::Foundation::HWND;
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED,
};
use windows::Win32::System::Variant::{
    VARENUM, VARIANT, VARIANT_0, VARIANT_0_0, VARIANT_0_0_0, VT_I4,
};
use windows::Win32::UI::Accessibility::{
    CUIAutomation, IUIAutomation, IUIAutomationValuePattern, TreeScope_Descendants,
    UIA_ControlTypePropertyId, UIA_EditControlTypeId, UIA_ValuePatternId,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_KEYUP, VK_RETURN,
};
use windows::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, SetForegroundWindow};
use windows::core::BSTR;

/// Build a VT_I4 VARIANT — mirrors the helper in shell_windows.rs.
///
/// # Safety
///
/// The returned VARIANT is a simple numeric VT_I4; no interface pointer
/// or allocation is involved, so no additional cleanup is required.
unsafe fn variant_i4(n: i32) -> VARIANT {
    use core::mem::ManuallyDrop;
    VARIANT {
        Anonymous: VARIANT_0 {
            Anonymous: ManuallyDrop::new(VARIANT_0_0 {
                vt: VARENUM(VT_I4.0),
                wReserved1: 0,
                wReserved2: 0,
                wReserved3: 0,
                Anonymous: VARIANT_0_0_0 { lVal: n },
            }),
        },
    }
}

fn main() -> windows::core::Result<()> {
    let path = env::args()
        .nth(1)
        .expect("Usage: uia_spike <path-to-navigate-to>");
    println!("UIA spike — will attempt to navigate a file dialog to: {path}");
    println!("You have 3 seconds to bring the target dialog to foreground...");

    // Ignore S_FALSE ("already initialized on this thread") — both are fine.
    let _ = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
    std::thread::sleep(Duration::from_secs(3));

    let hwnd: HWND = unsafe { GetForegroundWindow() };
    println!("Foreground HWND: {hwnd:?}");
    if hwnd.0.is_null() {
        eprintln!("No foreground window. Aborting.");
        return Ok(());
    }

    let uia: IUIAutomation =
        unsafe { CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER)? };
    let root = unsafe { uia.ElementFromHandle(hwnd)? };

    // Build condition: ControlType == Edit
    let control_type_val = unsafe { variant_i4(UIA_EditControlTypeId.0) };
    let cond =
        unsafe { uia.CreatePropertyCondition(UIA_ControlTypePropertyId, &control_type_val)? };
    let edits = unsafe { root.FindAll(TreeScope_Descendants, &cond)? };
    let count = unsafe { edits.Length()? };
    println!("\n=== Edit descendants: {count} found ===");
    for i in 0..count {
        let edit = unsafe { edits.GetElement(i)? };
        let aid = unsafe { edit.CurrentAutomationId().unwrap_or_default() };
        let name = unsafe { edit.CurrentName().unwrap_or_default() };
        println!(
            "  [{i}] AutomationId={:?} Name={:?}",
            aid.to_string(),
            name.to_string()
        );
    }

    println!("\n=== Attempting navigate on each Edit ===");
    for i in 0..count {
        let edit = unsafe { edits.GetElement(i)? };
        let aid = unsafe {
            edit.CurrentAutomationId()
                .unwrap_or_default()
                .to_string()
        };
        let name = unsafe { edit.CurrentName().unwrap_or_default().to_string() };
        println!("\n--- Attempt [{i}] AutomationId={aid} Name={name} ---");

        // Try to get ValuePattern
        let vp: windows::core::Result<IUIAutomationValuePattern> =
            unsafe { edit.GetCurrentPatternAs(UIA_ValuePatternId) };
        let Ok(vp) = vp else {
            println!("  no ValuePattern — skipping");
            continue;
        };

        let path_bstr = BSTR::from(path.as_str());
        if let Err(e) = unsafe { vp.SetValue(&path_bstr) } {
            println!("  SetValue failed: {e:?}");
            continue;
        }
        println!("  SetValue OK — submitting with Enter...");

        let _ = unsafe { SetForegroundWindow(hwnd) };
        std::thread::sleep(Duration::from_millis(60));

        let inputs = [
            INPUT {
                r#type: INPUT_KEYBOARD,
                Anonymous: INPUT_0 {
                    ki: KEYBDINPUT {
                        wVk: VK_RETURN,
                        wScan: 0,
                        dwFlags: Default::default(),
                        time: 0,
                        dwExtraInfo: 0,
                    },
                },
            },
            INPUT {
                r#type: INPUT_KEYBOARD,
                Anonymous: INPUT_0 {
                    ki: KEYBDINPUT {
                        wVk: VK_RETURN,
                        wScan: 0,
                        dwFlags: KEYEVENTF_KEYUP,
                        time: 0,
                        dwExtraInfo: 0,
                    },
                },
            },
        ];
        let sent = unsafe { SendInput(&inputs, std::mem::size_of::<INPUT>() as i32) };
        println!("  SendInput sent {sent} events");
        std::thread::sleep(Duration::from_millis(800));

        println!("  ^ observe the dialog. If it navigated to {path}, THIS is the selector.");
        println!("  Press Enter to continue to next candidate (or Ctrl+C to stop)...");
        let mut buf = String::new();
        std::io::stdin().read_line(&mut buf).ok();
    }

    println!("\n=== Done ===");
    Ok(())
}
