import Foundation
import Sparkle

/// Sparkle delegate that provides the appcast URL programmatically
/// so the update path works identically in:
///
/// - The production `.app` bundle, which has `SUFeedURL` baked into
///   `Info.plist` by `make-release.sh`.
/// - A bare `swift run` dev binary, which has no Info.plist and
///   therefore no `SUFeedURL` — without this delegate the dev
///   workflow's "Check for Updates…" silently no-ops because
///   Sparkle has no URL to fetch.
///
/// The delegate's `feedURLString(for:)` takes precedence over
/// `SUFeedURL` in `Info.plist`, so this is the single source of
/// truth either way. If the URL ever changes, only this constant
/// needs to update (matched by the same string in
/// `make-release.sh`'s Info.plist generator).
///
/// The class needs to be retained for the lifetime of the
/// `SPUStandardUpdaterController` — `ApfsFastindexApp` holds it
/// as a stored property.
final class UpdaterDelegate: NSObject, SPUUpdaterDelegate {
    /// Appcast URL on the upstream `apfs-fastindex` repo's main
    /// branch. Forks that want their own update channel can
    /// replace this string and rebuild.
    static let feedURLString =
        "https://raw.githubusercontent.com/NicoNekoru/apfs-fastindex/main/appcast.xml"

    func feedURLString(for updater: SPUUpdater) -> String? {
        Self.feedURLString
    }
}
