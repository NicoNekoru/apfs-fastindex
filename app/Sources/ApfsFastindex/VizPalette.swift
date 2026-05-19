import SwiftUI

/// Single-source-of-truth palette for the native renderer.
/// Lives in its own file (instead of bundled with the toolbar
/// view) so the various SwiftUI shells can pull from it without
/// pulling in a window-state import too.
///
/// Colours were originally calibrated to match the bundled HTML
/// viz's `--bg` / `--panel` / `--accent` variables so the
/// WKWebView pane and the AppKit chrome read as one surface.
/// The HTML viz is gone in phase 6 but the palette stays —
/// no reason to redo the dark-mode chrome from scratch.
enum VizPalette {
    static let bg      = Color(red: 0x0f/255.0, green: 0x11/255.0, blue: 0x15/255.0)
    static let panel   = Color(red: 0x1a/255.0, green: 0x1d/255.0, blue: 0x24/255.0)
    static let border  = Color(red: 0x2a/255.0, green: 0x2e/255.0, blue: 0x38/255.0)
    static let text    = Color(red: 0xe4/255.0, green: 0xe7/255.0, blue: 0xee/255.0)
    static let muted   = Color(red: 0x8b/255.0, green: 0x93/255.0, blue: 0xa5/255.0)
    static let accent  = Color(red: 0x4f/255.0, green: 0x8c/255.0, blue: 0xff/255.0)
    static let warning = Color(red: 0xfb/255.0, green: 0xbf/255.0, blue: 0x24/255.0)
}
