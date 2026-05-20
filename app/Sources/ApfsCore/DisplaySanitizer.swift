import Foundation

/// Sanitiser for parser-supplied filesystem names before they
/// hit the UI. The Rust scanner walks raw on-disk bytes — APFS
/// stores names as UTF-8 but doesn't normalize, case-fold, or
/// reject control codes — so a crafted volume can carry names
/// with:
///
/// - C0 controls (U+0000 .. U+001F) — including NUL, which
///   truncates `String(cString:)` in some toolchains and
///   misrenders in others.
/// - C1 controls (U+0080 .. U+009F).
/// - The bidi-override block (U+202A .. U+202E and the
///   isolate-and-pop sequence U+2066 .. U+2069). These flip
///   render order so an entry named `evil\u{202E}gpj.exe`
///   shows up as `evilexe.jpg` in the menu while the action
///   targets `evil...exe`. Round-2 audit's App #2 spoofing
///   vector.
///
/// `sanitiseDisplay` replaces every offending code point with
/// U+FFFD REPLACEMENT CHARACTER. We *don't* drop the bytes
/// silently — a visible replacement marker lets the user see
/// that the name carried something unprintable, which is the
/// signal we want.
///
/// Caller responsibilities:
///   - Use the sanitised string everywhere a parser-derived
///     name reaches the UI (menu titles, tooltips, labels,
///     status bar).
///   - For pasteboard writes (Copy Path), use the sanitised
///     form too — pasting raw control bytes into a terminal
///     is the classic next-step in this attack chain.
///   - Don't pass sanitised strings to `NSWorkspace` /
///     `URL(fileURLWithPath:)` — those need the *real* bytes,
///     and containment-checking lives in `PathContainment`.
public enum DisplaySanitizer {

    /// Returns a copy of `s` with control codes and bidi
    /// overrides replaced by U+FFFD. Idempotent.
    public static func sanitiseDisplay(_ s: String) -> String {
        var out = String()
        out.reserveCapacity(s.count)
        for scalar in s.unicodeScalars {
            if Self.isUnsafe(scalar) {
                out.unicodeScalars.append("\u{FFFD}")
            } else {
                out.unicodeScalars.append(scalar)
            }
        }
        return out
    }

    private static func isUnsafe(_ scalar: Unicode.Scalar) -> Bool {
        let v = scalar.value
        // C0 controls including NUL, excluding tab/newline/
        // carriage-return which we let through so multi-line
        // names render approximately (still rare on real
        // volumes, but not security-relevant).
        if v == 0x09 || v == 0x0A || v == 0x0D {
            return false
        }
        if v < 0x20 {
            return true
        }
        // C1 controls.
        if (0x7F...0x9F).contains(v) {
            return true
        }
        // Bidi override + isolate block.
        if (0x202A...0x202E).contains(v) || (0x2066...0x2069).contains(v) {
            return true
        }
        return false
    }
}
