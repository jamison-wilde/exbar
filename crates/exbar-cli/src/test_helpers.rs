//! Shared test helpers — `TestDeps`, builders, and Arc-bridging
//! newtypes for constructing a `ToolbarState` with mocks. Compiled
//! only under `#[cfg(test)]`.

use crate::clipboard::{Clipboard, test_mocks::MockClipboard};
use crate::config::{Config, ConfigStore, FolderEntry, test_mocks::MockConfigStore};
use crate::dialog_nav::{DialogNavigator, test_mocks::MockDialogNavigator};
use crate::dragdrop::{FileOperator, test_mocks::MockFileOp};
use crate::error::ExbarResult;
use crate::layout::{ButtonLayout, Rect};
use crate::picker::{FolderPicker, test_mocks::MockFolderPicker};
use crate::shell_windows::test_mocks::MockShellBrowser;
use crate::toolbar::ToolbarState;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::{Arc, Mutex};

// Newtypes bridging Arc<Concrete> → Box<dyn Trait>.
pub struct PickerArc(pub Arc<MockFolderPicker>);
impl FolderPicker for PickerArc {
    fn pick_folder(&self) -> Option<PathBuf> {
        self.0.pick_folder()
    }
}
pub struct ClipArc(pub Arc<MockClipboard>);
impl Clipboard for ClipArc {
    fn set_text(&self, t: &str) -> ExbarResult<()> {
        self.0.set_text(t)
    }
}
pub struct CfgArc(pub Arc<MockConfigStore>);
impl ConfigStore for CfgArc {
    fn load(&self) -> Option<Config> {
        self.0.load()
    }
    fn save(&self, c: &Config) -> ExbarResult<()> {
        self.0.save(c)
    }
}

/// Rc-bridging newtype so `MockDialogNavigator` (with `RefCell` interior
/// mutability, which is not `Sync`) can be shared between `TestDeps` and
/// `Box<dyn DialogNavigator>`. All tests are single-threaded, so `Rc` is safe.
pub struct DlgNavRc(pub Rc<MockDialogNavigator>);
impl DialogNavigator for DlgNavRc {
    fn navigate(
        &self,
        dialog_hwnd: windows::Win32::Foundation::HWND,
        path: &std::path::Path,
    ) -> ExbarResult<()> {
        self.0.navigate(dialog_hwnd, path)
    }
}

pub struct TestDeps {
    pub navigate_calls: Arc<Mutex<Vec<(isize, PathBuf)>>>,
    pub new_tab_calls: Arc<Mutex<Vec<(isize, PathBuf, u32)>>>,
    pub new_window_calls: Arc<Mutex<Vec<PathBuf>>>,
    pub picker: Arc<MockFolderPicker>,
    pub file_op: Arc<MockFileOp>,
    pub clipboard: Arc<MockClipboard>,
    pub cfg_store: Arc<MockConfigStore>,
    pub dialog_nav: Rc<MockDialogNavigator>,
}

pub fn mk_deps() -> TestDeps {
    TestDeps {
        navigate_calls: Arc::default(),
        new_tab_calls: Arc::default(),
        new_window_calls: Arc::default(),
        picker: Arc::new(MockFolderPicker::default()),
        file_op: Arc::new(MockFileOp::default()),
        clipboard: Arc::new(MockClipboard::default()),
        cfg_store: Arc::new(MockConfigStore::default()),
        dialog_nav: Rc::new(MockDialogNavigator::default()),
    }
}

pub fn make_test_state(deps: &TestDeps, config: Option<Config>) -> ToolbarState {
    let shell = MockShellBrowser {
        navigate_calls: Arc::clone(&deps.navigate_calls),
        new_tab_calls: Arc::clone(&deps.new_tab_calls),
        new_window_calls: Arc::clone(&deps.new_window_calls),
    };
    ToolbarState::with_deps(
        96,
        config,
        Box::new(shell),
        Box::new(PickerArc(deps.picker.clone())),
        deps.file_op.clone() as Arc<dyn FileOperator>,
        Box::new(ClipArc(deps.clipboard.clone())),
        Box::new(CfgArc(deps.cfg_store.clone())),
        Box::new(DlgNavRc(Rc::clone(&deps.dialog_nav))),
    )
}

pub fn mk_add_button() -> ButtonLayout {
    ButtonLayout {
        rect: Rect {
            left: 0,
            top: 0,
            right: 40,
            bottom: 28,
        },
        folder: FolderEntry {
            name: "+".into(),
            path: String::new(),
            icon: None,
        },
        is_add: true,
    }
}

pub fn mk_folder_button(name: &str, path: &str, left: i32) -> ButtonLayout {
    ButtonLayout {
        rect: Rect {
            left,
            top: 0,
            right: left + 90,
            bottom: 28,
        },
        folder: FolderEntry {
            name: name.into(),
            path: path.into(),
            icon: None,
        },
        is_add: false,
    }
}

pub fn mk_config_with_folders(entries: &[(&str, &str)]) -> Config {
    // `Config` deliberately doesn't impl Default (see config.rs comment),
    // so build via JSON round-trip — guarantees serde-side defaults
    // (opacity, layout, timeout, log_level) match the production path.
    let folders_json: Vec<String> = entries
        .iter()
        .map(|(name, path)| {
            let escaped_path = path.replace('\\', "\\\\");
            format!(r#"{{"name":"{name}","path":"{escaped_path}"}}"#)
        })
        .collect();
    let json = format!(r#"{{"folders":[{}]}}"#, folders_json.join(","));
    Config::from_str(&json).expect("test config json parses")
}
