//! Main-thread AppKit application menus. The menu outlives windows, just like
//! the link and tray; reopening replaces only its repaint callback. The
//! traffic lights are placed by fastframe-macos.

use std::cell::RefCell;
use std::ffi::CString;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock, Mutex};

use objc2::Encode;
use objc2::runtime::{AnyClass, AnyObject, Bool, Imp, MethodImplementation, Sel};
use objc2_app_kit::{NSApplication, NSText};
use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem as Native, Submenu};

use crate::model::{Action, Dialog, Page};

thread_local! {
    static MENU: RefCell<Option<Menu>> = const { RefCell::new(None) };
}
static EVENTS: Mutex<Vec<String>> = Mutex::new(Vec::new());
static EDIT_EVENTS: LazyLock<Arc<Mutex<Vec<egui::Event>>>> =
    LazyLock::new(|| Arc::new(Mutex::new(Vec::new())));
static REPAINT: Mutex<Option<egui::Context>> = Mutex::new(None);
/// A Dock click while no window shows, replayed as `Action::ShowWindow` by
/// `drain` — ZapFast hides to the Dock instead of a status item.
static REOPEN: AtomicBool = AtomicBool::new(false);

/// Menu edits must reach the input before egui processes focus and selection.
struct MenuInput(Arc<Mutex<Vec<egui::Event>>>);

impl egui::plugin::Plugin for MenuInput {
    fn debug_name(&self) -> &'static str {
        "zapfast-macos-menu"
    }

    fn input_hook(&mut self, _ctx: &egui::Context, input: &mut egui::RawInput) {
        input
            .events
            .extend(self.0.lock().unwrap_or_else(|p| p.into_inner()).drain(..));
    }
}

fn item(id: &str, text: &str, shortcut: Option<&str>) -> MenuItem {
    MenuItem::with_id(
        id,
        text,
        true,
        shortcut.map(|key| key.parse().expect("menu shortcut")),
    )
}

fn build_menu() -> tray_icon::menu::Result<Menu> {
    let menu = Menu::new();
    let app = Submenu::new("ZapFast", true);
    app.append_items(&[
        &item("about", "About ZapFast", None),
        &Native::separator(),
        &item("settings", "Settings…", Some("Super+Comma")),
        &Native::separator(),
        &Native::services(None),
        &Native::separator(),
        &Native::hide(Some("Hide ZapFast")),
        &Native::hide_others(None),
        &Native::show_all(None),
        &Native::separator(),
        &item("quit", "Quit ZapFast", Some("Super+KeyQ")),
    ])?;
    let file = Submenu::new("File", true);
    file.append_items(&[
        &item("new", "New Chat…", Some("Super+KeyN")),
        &Native::separator(),
        &item("close", "Close Window", Some("Super+KeyW")),
    ])?;
    let edit = Submenu::new("Edit", true);
    // Winit's view is not an NSTextView: AppKit's copy:/undo: selectors
    // cannot edit egui text. Send the same events as its keyboard shortcuts.
    edit.append_items(&[
        &item("undo", "Undo", Some("Super+KeyZ")),
        &item("redo", "Redo", Some("Super+Shift+KeyZ")),
        &Native::separator(),
        &item("cut", "Cut", Some("Super+KeyX")),
        &item("copy", "Copy", Some("Super+KeyC")),
        &item("paste", "Paste", Some("Super+KeyV")),
        &item("select-all", "Select All", Some("Super+KeyA")),
        &Native::separator(),
        &item("search", "Find…", Some("Super+KeyF")),
    ])?;
    let view = Submenu::new("View", true);
    view.append_items(&[
        &item("sidebar", "Toggle Sidebar", Some("Super+KeyB")),
        &Native::separator(),
        &item("zoom-in", "Zoom In", Some("Super+Equal")),
        &item("zoom-out", "Zoom Out", Some("Super+Minus")),
        &item("zoom-reset", "Actual Size", Some("Super+Digit0")),
        &Native::separator(),
        &Native::fullscreen(None),
    ])?;
    let window = Submenu::new("Window", true);
    window.append_items(&[
        &Native::minimize(None),
        &Native::maximize(Some("Zoom")),
        &Native::separator(),
        &item("show-window", "Show ZapFast", None),
    ])?;
    let help = Submenu::new("Help", true);
    help.append_items(&[
        &item("shortcuts", "Keyboard Shortcuts", Some("Super+Slash")),
        &item("help", "ZapFast Help", None),
    ])?;
    menu.append_items(&[&app, &file, &edit, &view, &window, &help])?;
    window.set_as_windows_menu_for_nsapp();
    help.set_as_help_menu_for_nsapp();
    Ok(menu)
}

/// Works around an AppKit bug that aborts the process when a window closes on
/// a Mac with a Touch Bar. The Touch Bar finder observes `nextResponder` on
/// each responder in the key window's chain and invalidates the observations
/// from a display-cycle callback. Window teardown can remove a registration
/// first, so the finder's `removeObserver` throws NSRangeException ("not
/// registered as an observer"), seen as EXC_BREAKPOINT on close
/// (emilk/egui#2768). Swallowing that one exception inside `invalidate` makes
/// the stale removal the no-op it was meant to be; the rest of the original
/// implementation still runs. The class is private: if Apple renames it the
/// guard is simply not installed.
fn guard_touch_bar_finder() {
    type Invalidate = unsafe extern "C-unwind" fn(*mut AnyObject, Sel);
    static ORIGINAL: std::sync::OnceLock<Invalidate> = std::sync::OnceLock::new();

    // The @try/@catch is compiled C (build_support/touch_bar_guard.m): an
    // Objective-C exception has to unwind through every frame up to the
    // catcher, and Rust frames emit no unwind tables under the release
    // profile's `panic = "abort"`, so `objc2::exception::catch` aborts the
    // process before it can see the exception.
    unsafe extern "C" {
        /// Returns false when it swallowed the stale-observer NSRangeException.
        fn zapfast_call_swallowing_range_error(
            imp: Invalidate,
            object: *mut AnyObject,
            selector: Sel,
        ) -> bool;
    }

    unsafe extern "C-unwind" fn guarded(this: *mut AnyObject, cmd: Sel) {
        let Some(original) = ORIGINAL.get() else {
            return;
        };
        if unsafe { !zapfast_call_swallowing_range_error(*original, this, cmd) } {
            log::warn!("Touch Bar finder hit a stale responder registration; ignored it");
        }
    }

    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| unsafe {
        let Some(class) = AnyClass::get(c"_NSTouchBarFinderObservation") else {
            return;
        };
        let Some(method) = class.instance_method(objc2::sel!(invalidate)) else {
            return;
        };
        let _ = ORIGINAL.set(std::mem::transmute::<Imp, Invalidate>(
            method.implementation(),
        ));
        method.set_implementation(std::mem::transmute::<Invalidate, Imp>(guarded));
    });
}

pub fn attach(ctx: &egui::Context) {
    ctx.add_plugin(MenuInput(Arc::clone(&EDIT_EVENTS)));
    // Layout tests use headless contexts on test threads, without an NSApp.
    if objc2::MainThreadMarker::new().is_none() {
        return;
    }
    guard_touch_bar_finder();
    install_reopen_handler();
    *REPAINT.lock().unwrap_or_else(|p| p.into_inner()) = Some(ctx.clone());
    MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
        // The tray's menu shares muda's one handler; its ids are its own.
        if fastframe_tray::claim_menu_event(&event.id.0) {
            return;
        }
        if native_edit(&event.id.0) {
            return;
        }
        if let Some(edit) = edit_event(&event.id.0) {
            EDIT_EVENTS
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .push(edit);
        } else {
            EVENTS
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .push(event.id.0);
        }
        if let Some(ctx) = &*REPAINT.lock().unwrap_or_else(|p| p.into_inner()) {
            ctx.request_repaint();
        }
    }));
    MENU.with_borrow_mut(|menu| {
        if menu.is_none() {
            match build_menu() {
                Ok(created) => *menu = Some(created),
                Err(error) => log::warn!("could not create the application menu: {error}"),
            }
        }
        if let Some(menu) = menu {
            menu.init_for_nsapp();
        }
    });
    // Demo windows have no tray to activate the app. This also brings a newly
    // recreated window forward after a menu command while running headless.
    ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
}

/// A native file dialog runs a modal loop, so its text fields must receive
/// editing commands immediately instead of leaving them queued for egui.
fn native_edit(id: &str) -> bool {
    use objc2::sel;
    let selector = match id {
        "copy" => sel!(copy:),
        "cut" => sel!(cut:),
        "paste" => sel!(paste:),
        "undo" => sel!(undo:),
        "redo" => sel!(redo:),
        "select-all" => sel!(selectAll:),
        _ => return false,
    };
    let Some(main) = objc2::MainThreadMarker::new() else {
        return false;
    };
    let app = NSApplication::sharedApplication(main);
    let Some(responder) = app.keyWindow().and_then(|window| window.firstResponder()) else {
        return false;
    };
    // Only native text editors (including a file dialog's field editor) can
    // handle these selectors. GL views and AccessKit's dynamically subclassed
    // WinitView must use egui, regardless of their Objective-C class name.
    if responder.downcast_ref::<NSText>().is_none() {
        return false;
    }
    // SAFETY: standard AppKit editing selectors; nil target walks the native
    // responder chain, and these actions accept a nil sender.
    unsafe { app.sendAction_to_from(selector, None, None) }
}

fn edit_event(id: &str) -> Option<egui::Event> {
    match id {
        "copy" => return Some(egui::Event::Copy),
        "cut" => return Some(egui::Event::Cut),
        "paste" => {
            return Some(egui::Event::Paste(
                arboard::Clipboard::new()
                    .ok()
                    .and_then(|mut clipboard| clipboard.get_text().ok())
                    .unwrap_or_default(),
            ));
        }
        _ => {}
    }
    let (key, shift) = match id {
        "undo" => (egui::Key::Z, false),
        "redo" => (egui::Key::Z, true),
        "select-all" => (egui::Key::A, false),
        _ => return None,
    };
    Some(egui::Event::Key {
        key,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers: egui::Modifiers {
            command: true,
            mac_cmd: true,
            shift,
            ..Default::default()
        },
    })
}

fn action(id: &str) -> Option<Action> {
    Some(match id {
        "about" => Action::ShowDialog(Dialog::About),
        "settings" => Action::Open(Page::Settings),
        "new" => Action::ShowDialog(Dialog::NewChat),
        "close" => Action::CloseWindow,
        "quit" => Action::Quit,
        "search" => Action::FocusSearch,
        "sidebar" => Action::ToggleSidebar,
        "zoom-in" => Action::ZoomBy(0.1),
        "zoom-out" => Action::ZoomBy(-0.1),
        "zoom-reset" => Action::ResetZoom,
        "shortcuts" => Action::ShowDialog(Dialog::Shortcuts),
        "help" => Action::OpenUrl("https://zapfast.rocks/using-zapfast/".into()),
        "show-window" => Action::ShowWindow,
        _ => return None,
    })
}

/// A Dock click, asking for the app back. `has_visible_windows` is not the
/// question it sounds like: a window sitting in the Dock counts as visible,
/// which is exactly the case that needs help, so the flag is not consulted
/// (Spotifast ec75951). Asking for a window that is already up costs a
/// focus and nothing else.
extern "C-unwind" fn application_should_handle_reopen(
    _delegate: *mut AnyObject,
    _selector: Sel,
    _application: *mut NSApplication,
    _has_visible_windows: Bool,
) -> Bool {
    REOPEN.store(true, Ordering::Relaxed);
    // A minimized window draws no frames; without a wake nobody reads the
    // flag.
    if let Some(ctx) = &*REPAINT.lock().unwrap_or_else(|p| p.into_inner()) {
        ctx.request_repaint();
    }
    Bool::YES
}

/// Adds `applicationShouldHandleReopen:hasVisibleWindows:` to winit's
/// application delegate, unless it already answers it.
fn install_reopen_handler() {
    let Some(mtm) = objc2::MainThreadMarker::new() else {
        return;
    };
    let app = NSApplication::sharedApplication(mtm);
    let Some(delegate) = app.delegate() else {
        log::warn!("the macOS application delegate is unavailable");
        return;
    };
    let delegate: &AnyObject = AsRef::<AnyObject>::as_ref(&*delegate);
    let class = delegate.class();
    let selector = objc2::sel!(applicationShouldHandleReopen:hasVisibleWindows:);
    if class.responds_to(selector) {
        return;
    }
    let implementation: extern "C-unwind" fn(
        *mut AnyObject,
        Sel,
        *mut NSApplication,
        Bool,
    ) -> Bool = application_should_handle_reopen;
    let Ok(types) = CString::new(format!("{}@:@{}", Bool::ENCODING, Bool::ENCODING)) else {
        return;
    };
    // SAFETY: the implementation's signature matches the type encoding, and
    // the selector is not yet on the class, so nothing is replaced.
    let installed = unsafe {
        objc2::ffi::class_addMethod(
            std::ptr::from_ref::<AnyClass>(class).cast_mut(),
            selector,
            implementation.__imp(),
            types.as_ptr(),
        )
    };
    if !installed.as_bool() {
        log::warn!("the macOS Dock reopen handler could not be installed");
    }
}

pub fn drain(hidden: bool) -> Vec<Action> {
    if hidden {
        EDIT_EVENTS
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clear();
    }
    let events = std::mem::take(&mut *EVENTS.lock().unwrap_or_else(|p| p.into_inner()));
    let mut actions = Vec::new();
    if REOPEN.swap(false, Ordering::Relaxed) {
        actions.push(Action::ShowWindow);
    }
    for id in events {
        if let Some(action) = action(&id) {
            if hidden
                && matches!(
                    action,
                    Action::ShowDialog(_) | Action::Open(_) | Action::FocusSearch
                )
            {
                actions.push(Action::ShowWindow);
            }
            actions.push(action);
        }
    }
    actions
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn menu_edits_reach_the_focused_text_field_before_the_pass() {
        let ctx = egui::Context::default();
        let events = Arc::new(Mutex::new(Vec::new()));
        ctx.add_plugin(MenuInput(Arc::clone(&events)));
        let id = egui::Id::new("menu-edit-fixture");
        let mut text = String::from("draft");
        let mut frame = |event: Option<egui::Event>| {
            events.lock().unwrap().extend(event);
            ctx.memory_mut(|memory| memory.request_focus(id));
            let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
                ui.add(egui::TextEdit::singleline(&mut text).id(id));
            });
            output.textures_delta.clear();
            output
        };
        let _ = frame(None);
        let _ = frame(edit_event("select-all"));
        let output = frame(edit_event("cut"));
        assert!(output.platform_output.commands.iter().any(|command| {
            matches!(command, egui::OutputCommand::CopyText(value) if value == "draft")
        }));
        let _ = frame(Some(egui::Event::Paste("replacement".into())));
        let _ = frame(edit_event("select-all"));
        let output = frame(edit_event("copy"));
        assert!(output.platform_output.commands.iter().any(|command| {
            matches!(command, egui::OutputCommand::CopyText(value) if value == "replacement")
        }));
        assert_eq!(text, "replacement");
        assert!(events.lock().unwrap().is_empty());
    }

    #[test]
    fn menu_uses_the_apps_close_quit_and_edit_paths() {
        assert!(matches!(action("close"), Some(Action::CloseWindow)));
        assert!(matches!(action("quit"), Some(Action::Quit)));
        assert!(matches!(action("show-window"), Some(Action::ShowWindow)));
        assert!(matches!(edit_event("copy"), Some(egui::Event::Copy)));
        assert!(
            matches!(edit_event("redo"), Some(egui::Event::Key { key: egui::Key::Z, modifiers, .. }) if modifiers.command && modifiers.shift)
        );
        for shortcut in [
            "Super+KeyN",
            "Super+Comma",
            "Super+Equal",
            "Super+Digit0",
            "Super+Shift+KeyZ",
        ] {
            shortcut
                .parse::<tray_icon::menu::accelerator::Accelerator>()
                .unwrap();
        }
    }
}
