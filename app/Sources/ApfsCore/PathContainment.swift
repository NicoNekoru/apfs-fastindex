import Foundation

/// Path-containment helpers used by the SwiftUI app and exercised
/// by the FFI test runner. Lives in its own target so both
/// callers can depend on it without entangling SwiftUI / AppKit.
public enum PathContainment {

    /// Resolve a relative entry path without letting file actions escape
    /// the scan root. Compare components under NFC and the volume's case
    /// rules, then check resolved ancestor symlinks. The final component
    /// remains unresolved so a symlink itself can be revealed or trashed.
    /// This is a point-in-time check. Callers must recheck before acting.
    public static func resolveContained(
        scanRoot: String,
        relative: String
    ) -> String? {
        guard !scanRoot.isEmpty, !scanRoot.utf8.contains(0),
            !relative.hasPrefix("/"), !relative.utf8.contains(0)
        else { return nil }
        let joinedRaw = relative.isEmpty
            ? scanRoot
            : (scanRoot as NSString).appendingPathComponent(relative)
        let rootURL = URL(fileURLWithPath: scanRoot).standardizedFileURL
        let candidateURL = URL(fileURLWithPath: joinedRaw).standardizedFileURL
        let caseInsensitive = volumeIsCaseInsensitive(at: rootURL)
        guard contains(root: rootURL, candidate: candidateURL, caseInsensitive: caseInsensitive) else {
            return nil
        }
        if candidateURL.pathComponents.count > rootURL.pathComponents.count {
            let resolvedRoot = rootURL.resolvingSymlinksInPath()
            let resolvedParent = candidateURL.deletingLastPathComponent().resolvingSymlinksInPath()
            guard contains(root: resolvedRoot, candidate: resolvedParent, caseInsensitive: caseInsensitive) else {
                return nil
            }
        }
        return candidateURL.path
    }

    private static func contains(root: URL, candidate: URL, caseInsensitive: Bool) -> Bool {
        let rootComponents = root.pathComponents
        let candidateComponents = candidate.pathComponents
        guard candidateComponents.count >= rootComponents.count else { return false }
        return zip(rootComponents, candidateComponents).allSatisfy {
            equalForContainment($0.0, $0.1, caseInsensitive: caseInsensitive)
        }
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
    /// On lookup failure, require exact case rather than accepting a
    /// differently-cased sibling on an unknown volume.
    public static func volumeIsCaseInsensitive(at url: URL) -> Bool {
        let keys: Set<URLResourceKey> = [.volumeSupportsCaseSensitiveNamesKey]
        if let values = try? url.resourceValues(forKeys: keys),
           let supportsCaseSensitive = values.volumeSupportsCaseSensitiveNames {
            return !supportsCaseSensitive
        }
        return false
    }
}
