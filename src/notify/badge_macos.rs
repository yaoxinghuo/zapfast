//! The unread count as a red bubble on the Dock icon.
//!
//! `NSApplication.dockTile.badgeLabel` is the whole implementation: the Dock
//! draws and clears the bubble itself and asks for no permission. The status
//! item is not involved, so the badge works with no menu-bar icon too.

use objc2::MainThreadMarker;
use objc2_app_kit::NSApplication;
use objc2_foundation::NSString;

/// Mirrors the unread total onto the Dock icon, skipping repeats because the
/// caller asks on every frame.
#[derive(Default)]
pub struct Badge {
    shown: Option<u32>,
}

impl Badge {
    /// Shows the unread total, clearing the bubble at zero.
    pub fn set(&mut self, count: u32) {
        if self.shown == Some(count) {
            return;
        }
        self.shown = Some(count);
        // AppKit allows the tile on the main thread only; demo and test runs
        // never reach it because their badge stays `None`.
        let Some(mtm) = MainThreadMarker::new() else {
            return;
        };
        let label = (count > 0).then(|| NSString::from_str(&count.to_string()));
        NSApplication::sharedApplication(mtm)
            .dockTile()
            .setBadgeLabel(label.as_deref());
    }
}
