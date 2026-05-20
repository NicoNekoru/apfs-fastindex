import Foundation

/// Path-containment helpers used by the SwiftUI app and exercised
/// by the FFI test runner. Lives in its own target so both
/// callers can depend on it without entangling SwiftUI / AppKit.
public enum PathContainment {

    /// Resolve `relative` against `scanRoot` and return the
    /// canonical absolute path **iff** the result stays
    /// contained inside `scanRoot`. Returns nil for paths that
    /// escape via `..` segments or absolute-path relative
    /// entries — those would otherwise direct context-menu
    /// actions (Reveal in Finder, Move to Trash, etc.) at
    /// arbitrary filesystem locations.
    ///
    /// Containment is verified by path-component prefix match
    /// after both sides are `.standardizedFileURL`-normalized.
    /// `standardizedFileURL` resolves `.` and `..` segments but
    /// does *not* follow symlinks — matches Finder semantics
    /// where Trash/Reveal operate on the symlink itself.
    ///
    /// Component-by-component comparison (not string prefix)
    /// prevents `/Users/kai` from accidentally accepting
    /// `/Users/kaiserwilhelm/...` as contained.
    public static func resolveContained(
        scanRoot: String,
        relative: String
    ) -> String? {
        guard !scanRoot.isEmpty else { return nil }
        let joinedRaw = relative.isEmpty
            ? scanRoot
            : (scanRoot as NSString).appendingPathComponent(relative)
        let rootURL = URL(fileURLWithPath: scanRoot).standardizedFileURL
        let candidateURL = URL(fileURLWithPath: joinedRaw).standardizedFileURL
        let rootComponents = rootURL.pathComponents
        let candidateComponents = candidateURL.pathComponents
        guard candidateComponents.count >= rootComponents.count,
              Array(candidateComponents.prefix(rootComponents.count)) == rootComponents
        else {
            return nil
        }
        return candidateURL.path
    }
}
