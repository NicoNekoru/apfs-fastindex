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
    /// after both sides are `.standardizedFileURL`-normalised
    /// AND Unicode-normalised (NFC) + case-folded on
    /// case-insensitive volumes. `standardizedFileURL` resolves
    /// `.` and `..` segments but does *not* follow symlinks —
    /// matches Finder semantics where Trash/Reveal operate on
    /// the symlink itself.
    ///
    /// Round-2 audit #N2: the previous straight `==` compare
    /// rejected real files when the scan root and an entry's
    /// path differed in Unicode normalisation (NFC vs NFD) or
    /// letter case. Most user volumes on macOS are
    /// case-insensitive, and APFS preserves the on-disk
    /// normalisation form — so a scan root captured NFC + an
    /// entry stored NFD would lose containment for paths that
    /// are in fact contained. Component-by-component comparison
    /// (not string prefix) still prevents `/Users/kai` from
    /// accidentally accepting `/Users/kaiserwilhelm/...` as
    /// contained.
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

        // Detect case sensitivity once per call (cheap — a
        // file-attribute lookup). On case-insensitive volumes
        // we lower-case before comparing; otherwise we only
        // NFC-normalise.
        let caseInsensitive = volumeIsCaseInsensitive(at: rootURL)

        guard candidateComponents.count >= rootComponents.count else {
            return nil
        }
        for (rootC, candC) in zip(rootComponents, candidateComponents.prefix(rootComponents.count)) {
            if !equalForContainment(rootC, candC, caseInsensitive: caseInsensitive) {
                return nil
            }
        }
        return candidateURL.path
    }

    /// Compare two single path components under NFC + optional
    /// case-folding. Pulled out so the test target can exercise
    /// the comparison in isolation.
    public static func equalForContainment(
        _ a: String,
        _ b: String,
        caseInsensitive: Bool
    ) -> Bool {
        // `precomposedStringWithCanonicalMapping` returns NFC.
        // We always normalise — the cost is a few hundred ns
        // per component on typical filenames, and pure-ASCII
        // paths short-circuit through Foundation's fast path.
        let aN = a.precomposedStringWithCanonicalMapping
        let bN = b.precomposedStringWithCanonicalMapping
        if caseInsensitive {
            return aN.caseInsensitiveCompare(bN) == .orderedSame
        }
        return aN == bN
    }

    /// Returns true iff the volume backing `url` reports as
    /// case-insensitive via `URLResourceValues`. On macOS the
    /// default APFS volume layout is case-insensitive; user-
    /// created case-sensitive volumes (APFS Case-Sensitive,
    /// HFS+ Case-Sensitive) return false.
    ///
    /// Defaults to `true` on lookup failure — most user volumes
    /// are case-insensitive, so the default matches the common
    /// case. A false positive here only changes containment
    /// from "fail-closed on case mismatch" to "accept case
    /// mismatch"; it doesn't open a new escape route because
    /// `..` normalisation happens earlier in
    /// `resolveContained`.
    public static func volumeIsCaseInsensitive(at url: URL) -> Bool {
        let keys: Set<URLResourceKey> = [.volumeSupportsCaseSensitiveNamesKey]
        if let values = try? url.resourceValues(forKeys: keys),
           let supportsCaseSensitive = values.volumeSupportsCaseSensitiveNames {
            return !supportsCaseSensitive
        }
        return true
    }
}
