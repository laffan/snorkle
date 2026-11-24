//! Keyboard shortcuts: the actions, their default bindings (`SHORTCUTS`),
//! validation and the user's bindings; arrow keys yield to text editing.

/// Browser keyboard shortcuts handled natively (the app's main menu,
/// `ios::menu`, with the content containers' key commands as a fallback,
/// `ios::keys`); the chrome UI performs them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Shortcut {
    Reload,
    NewTab,
    /// A new private tab (⌘⇧N, 2026-11-24).
    NewPrivateTab,
    CloseTab,
    /// Reopen the most recently closed tab (⌘⇧T).
    ReopenTab,
    /// Show or hide the tab sidebar.
    ToggleSidebar,
    /// Show or hide the right panel (headers, chat).
    ToggleRightPanel,
    /// Bookmark the current page, or un-bookmark a bookmark's tab.
    Bookmark,
    /// Show or hide images and video.
    ToggleImages,
    /// Find in page.
    Find,
    /// Next / previous match of the find bar (opens it if closed).
    FindNext,
    FindPrevious,
    /// Split the view with an empty tab: side by side (⌘S) or one above
    /// the other (⌘⇧S); in a split, change its layout.
    SplitColumns,
    SplitRows,
    /// The n-th item in the sidebar (pins, bookmarks, then tabs).
    Item(u8),
    /// The next / previous tab in the sidebar's order, wrapping (⌘⇧] and
    /// ⌘⇧[, as in Chrome and Safari).
    NextTab,
    PreviousTab,
    /// Back / forward in the tab's history (⌘← / ⌘→, 2026-11-15).
    Back,
    Forward,
    /// Detach the URL bar into the floating pill (⌘↑) or attach it along
    /// the bottom of the page (⌘↓), for the current tab.
    DetachUrlBar,
    AttachUrlBar,
    /// Night vision on or off (⌘⇧R, 2026-11-23): the same setting as
    /// Settings → General → Appearance and the eye at the sidebar's foot.
    NightVision,
    /// Show or hide the history sidebar (⌘⇧H, 2026-11-23).
    History,
    /// Send the page shown to the notebook as a link (⌘⇧D, 2026-11-24).
    NotebookPage,
    /// Quit the app (⌘Q, 2026-11-23; macOS only, see
    /// [`ShortcutDef::available`]). Handled natively, never sent to the
    /// chrome.
    Quit,
}

/// Arrow keys a binding may use, by name (the other keys are single
/// characters). While text is being edited (a text field or editable
/// region has keyboard focus, in a page or in the chrome;
/// [`set_text_editing`]) a binding on an arrow key is not taken, so ⌘←,
/// ⌘→, ⌘↑ and ⌘↓ keep moving the caret there.
pub const ARROW_KEYS: [&str; 4] = ["left", "right", "up", "down"];

pub fn is_arrow(key: &str) -> bool {
    ARROW_KEYS.contains(&key)
}

/// Whether text is being edited with keyboard focus (see [`ARROW_KEYS`]).
/// Set by the chrome, from its own focus and the pages' reports (C19).
pub(super) static TEXT_EDITING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub fn set_text_editing(on: bool) {
    TEXT_EDITING.store(on, std::sync::atomic::Ordering::Relaxed);
}

pub fn text_editing() -> bool {
    TEXT_EDITING.load(std::sync::atomic::Ordering::Relaxed)
}

/// Whether a binding is left to text editing right now.
pub fn yields_to_editing(binding: &KeyBinding) -> bool {
    is_arrow(&binding.key) && text_editing()
}

/// Modifier bits of a binding besides ⌘ (always required): Shift, Option,
/// Control (the chrome uses the same bits).
pub const MOD_SHIFT: u8 = 1;
pub const MOD_ALT: u8 = 2;
pub const MOD_CTRL: u8 = 4;

/// One native shortcut: a stable id (also the menu command's property
/// list, which UIKit requires to be unique per action), where it appears
/// in the menu bar, its title, its default key and modifiers, the action.
#[derive(Clone, Copy, Debug)]
pub struct ShortcutDef {
    pub id: &'static str,
    pub menu: ShortcutMenu,
    pub title: &'static str,
    pub key: &'static str,
    pub mods: u8,
    pub action: Shortcut,
}

pub(super) const fn def(
    id: &'static str,
    menu: ShortcutMenu,
    title: &'static str,
    key: &'static str,
    mods: u8,
    action: Shortcut,
) -> ShortcutDef {
    ShortcutDef { id, menu, title, key, mods, action }
}

/// Every native shortcut, with its default binding (⌘ plus `mods`).
pub const SHORTCUTS: &[ShortcutDef] = &[
    def("new_tab", ShortcutMenu::File, "New Tab", "t", 0, Shortcut::NewTab),
    def("new_private_tab", ShortcutMenu::File, "New Private Tab", "n", MOD_SHIFT, Shortcut::NewPrivateTab),
    def("reload", ShortcutMenu::View, "Reload Page", "r", 0, Shortcut::Reload),
    def("reopen_tab", ShortcutMenu::File, "Reopen Closed Tab", "t", MOD_SHIFT, Shortcut::ReopenTab),
    def("bookmark", ShortcutMenu::File, "Bookmark Page", "d", 0, Shortcut::Bookmark),
    def("split_columns", ShortcutMenu::View, "Split Side by Side", "s", 0, Shortcut::SplitColumns),
    def("split_rows", ShortcutMenu::View, "Split Top and Bottom", "s", MOD_SHIFT, Shortcut::SplitRows),
    def("close_tab", ShortcutMenu::Close, "Close Tab", "w", 0, Shortcut::CloseTab),
    def("find", ShortcutMenu::Find, "Find on Page", "f", 0, Shortcut::Find),
    def("find_next", ShortcutMenu::Find, "Find Next", "g", 0, Shortcut::FindNext),
    def("find_prev", ShortcutMenu::Find, "Find Previous", "g", MOD_SHIFT, Shortcut::FindPrevious),
    def("toggle_sidebar", ShortcutMenu::View, "Show or Hide Sidebar", "\\", 0, Shortcut::ToggleSidebar),
    def("toggle_right", ShortcutMenu::View, "Toggle Right Panel", "\\", MOD_SHIFT, Shortcut::ToggleRightPanel),
    def("toggle_images", ShortcutMenu::View, "Show or Hide Images", "i", 0, Shortcut::ToggleImages),
    def("item_1", ShortcutMenu::Tabs, "Go to Item 1", "1", 0, Shortcut::Item(1)),
    def("item_2", ShortcutMenu::Tabs, "Go to Item 2", "2", 0, Shortcut::Item(2)),
    def("item_3", ShortcutMenu::Tabs, "Go to Item 3", "3", 0, Shortcut::Item(3)),
    def("item_4", ShortcutMenu::Tabs, "Go to Item 4", "4", 0, Shortcut::Item(4)),
    def("item_5", ShortcutMenu::Tabs, "Go to Item 5", "5", 0, Shortcut::Item(5)),
    def("next_tab", ShortcutMenu::Tabs, "Next Tab", "]", MOD_SHIFT, Shortcut::NextTab),
    def("prev_tab", ShortcutMenu::Tabs, "Previous Tab", "[", MOD_SHIFT, Shortcut::PreviousTab),
    def("back", ShortcutMenu::View, "Back", "left", 0, Shortcut::Back),
    def("forward", ShortcutMenu::View, "Forward", "right", 0, Shortcut::Forward),
    def("detach_bar", ShortcutMenu::View, "Detach URL Bar", "up", 0, Shortcut::DetachUrlBar),
    def("attach_bar", ShortcutMenu::View, "Attach URL Bar", "down", 0, Shortcut::AttachUrlBar),
    def("night_vision", ShortcutMenu::View, "Night Vision", "r", MOD_SHIFT, Shortcut::NightVision),
    def("history", ShortcutMenu::View, "Show or Hide History", "h", MOD_SHIFT, Shortcut::History),
    def("notebook_page", ShortcutMenu::File, "Send Page to Notebook", "d", MOD_SHIFT, Shortcut::NotebookPage),
    def("quit", ShortcutMenu::File, "Quit Snorkle", "q", 0, Shortcut::Quit),
];

impl ShortcutDef {
    /// Whether this shortcut exists on the platform the app runs on. Quit
    /// is macOS only: iPadOS has no way for an app to quit itself (ending
    /// the process is what Apple's guidance tells apps not to do; owner
    /// decision 2026-11-23), so ⌘Q is neither offered nor reserved there.
    pub const fn available(&self) -> bool {
        available_on(self.action, cfg!(target_os = "macos"))
    }
}

pub(super) const fn available_on(action: Shortcut, macos: bool) -> bool {
    !matches!(action, Shortcut::Quit) || macos
}

/// Where a shortcut appears in the iPadOS menu bar.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShortcutMenu {
    File,
    /// Replaces the system's Close (⌘W closes a tab, not the window).
    Close,
    /// Replaces the system's Find menu.
    Find,
    View,
    /// A "Tabs" menu after View.
    Tabs,
}

impl Shortcut {
    /// Name passed to the chrome UI, which performs the action.
    pub fn name(self) -> String {
        match self {
            Shortcut::Reload => "reload".into(),
            Shortcut::NewTab => "new_tab".into(),
            Shortcut::NewPrivateTab => "new_private_tab".into(),
            Shortcut::CloseTab => "close_tab".into(),
            Shortcut::ReopenTab => "reopen_tab".into(),
            Shortcut::ToggleSidebar => "toggle_sidebar".into(),
            Shortcut::ToggleRightPanel => "toggle_right".into(),
            Shortcut::Bookmark => "bookmark".into(),
            Shortcut::ToggleImages => "toggle_images".into(),
            Shortcut::Find => "find".into(),
            Shortcut::FindNext => "find_next".into(),
            Shortcut::FindPrevious => "find_prev".into(),
            Shortcut::SplitColumns => "split_columns".into(),
            Shortcut::SplitRows => "split_rows".into(),
            Shortcut::Item(n) => format!("item:{n}"),
            Shortcut::NextTab => "next_tab".into(),
            Shortcut::PreviousTab => "prev_tab".into(),
            Shortcut::Back => "go_back".into(),
            Shortcut::Forward => "go_forward".into(),
            Shortcut::DetachUrlBar => "detach_bar".into(),
            Shortcut::AttachUrlBar => "attach_bar".into(),
            Shortcut::NightVision => "night_vision".into(),
            Shortcut::History => "history".into(),
            Shortcut::NotebookPage => "notebook_page".into(),
            Shortcut::Quit => "quit".into(),
        }
    }
}

/// Index of the shortcut with this id (a menu command's property list).
pub fn shortcut_index(id: &str) -> Option<usize> {
    SHORTCUTS.iter().position(|s| s.id == id)
}

/// A key binding: one key and modifier bits (⌘ always included).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeyBinding {
    pub key: String,
    pub mods: u8,
}

/// Keys a binding may use: letters, digits, a few punctuation keys (what
/// `UIKeyCommand` matches reliably as input) and the arrow keys, by name
/// ([`ARROW_KEYS`]).
pub fn validate_binding(key: &str, mods: u8) -> Option<KeyBinding> {
    let key = key.to_ascii_lowercase();
    let ok = is_arrow(&key)
        || (key.chars().count() == 1
            && key.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || "[]\\;',./-=`".contains(c)));
    (ok && mods & !(MOD_SHIFT | MOD_ALT | MOD_CTRL) == 0).then_some(KeyBinding { key, mods })
}

/// Applies the user's bindings (`id` → binding) over the defaults: every
/// shortcut's effective binding, in `SHORTCUTS` order. Unknown ids and
/// invalid keys are ignored; if two shortcuts would share a key, the
/// later override is refused (the error names both). Shortcuts this
/// platform does not have ([`ShortcutDef::available`]) keep their place
/// but reserve no keys.
pub fn effective_bindings(overrides: &[(String, String, u8)]) -> Result<Vec<KeyBinding>, String> {
    bindings_for(overrides, |d| d.available())
}

pub(super) fn bindings_for(
    overrides: &[(String, String, u8)],
    available: impl Fn(&ShortcutDef) -> bool,
) -> Result<Vec<KeyBinding>, String> {
    let mut out: Vec<KeyBinding> =
        SHORTCUTS.iter().map(|s| KeyBinding { key: s.key.to_string(), mods: s.mods }).collect();
    for (id, key, mods) in overrides {
        let (Some(i), Some(b)) = (shortcut_index(id), validate_binding(key, *mods)) else { continue };
        out[i] = b;
    }
    for i in (0..out.len()).filter(|i| available(&SHORTCUTS[*i])) {
        if let Some(j) = (0..i).find(|j| available(&SHORTCUTS[*j]) && out[*j] == out[i]) {
            return Err(format!("“{}” and “{}” use the same keys.", SHORTCUTS[j].title, SHORTCUTS[i].title));
        }
    }
    Ok(out)
}
