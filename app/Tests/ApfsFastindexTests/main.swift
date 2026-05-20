import Foundation
import ApfsCore
import CApfsFastindex

/// FFI-surface test runner for the Swift side. Defends the
/// security-fix commits dbc435f / fcdb597 / 1eacf54 from the
/// Swift consumer's perspective: the link stays clean, the
/// diagnostic surface (`apfs_last_error` / `apfs_log_path`)
/// round-trips, and a failed scan populates an error message
/// instead of returning a silent NULL.
///
/// Implemented as a `.executableTarget` rather than a SwiftPM
/// `.testTarget` because XCTest requires a full Xcode install
/// and the project's baseline tooling is Command-Line-Tools-only.
/// Exits 0 on success, 1 on the first failed assertion.

// MARK: - Tiny test scaffold

private var failures = 0
private var ran = 0

@discardableResult
func test(_ name: String, _ body: () -> Void) -> Bool {
    ran += 1
    let priorFailures = failures
    body()
    let passed = failures == priorFailures
    print("  \(passed ? "PASS" : "FAIL")  \(name)")
    return passed
}

func expect(_ condition: Bool, _ msg: @autoclosure () -> String,
            file: StaticString = #file, line: UInt = #line) {
    if !condition {
        failures += 1
        FileHandle.standardError.write(Data("    ASSERT  \(file):\(line)  \(msg())\n".utf8))
    }
}

// MARK: - FFI helpers

func readLastError() -> String? {
    guard let cstr = apfs_last_error() else { return nil }
    let s = String(cString: cstr)
    return s.isEmpty ? nil : s
}

func drainLastError() {
    _ = readLastError()
}

// MARK: - Tests

print("apfs-ffi-tests — running…")

test("apfs_hello returns 42") {
    expect(apfs_hello() == 42, "hello returned \(apfs_hello())")
}

test("apfs_version returns non-empty UTF-8") {
    guard let cstr = apfs_version() else {
        expect(false, "apfs_version returned NULL")
        return
    }
    let s = String(cString: cstr)
    expect(!s.isEmpty, "version was empty")
}

test("apfs_log_path resolves after install") {
    // Touch any FFI fn to install the panic hook lazily.
    _ = apfs_hello()
    guard let cstr = apfs_log_path() else {
        expect(false, "apfs_log_path returned NULL")
        return
    }
    let path = String(cString: cstr)
    expect(!path.isEmpty, "log path was empty")
    expect(path.hasSuffix("apfs-fastindex.log"),
           "log path should end with canonical filename; got: \(path)")
}

test("NULL-path scan populates last_error") {
    drainLastError()
    let handle = apfs_scan_directory_with_progress(
        nil, // path
        0,
        false,
        nil, // progress_fn
        nil  // userdata
    )
    expect(handle == nil, "NULL path must fail-closed (return NULL)")
    let err = readLastError()
    expect(err != nil, "last_error should be set after NULL-path scan")
    if let err {
        expect(err.lowercased().contains("null"),
               "error message should mention the cause; got: \(err)")
    }
}

test("last_error clears after read") {
    drainLastError()
    _ = apfs_scan_directory_with_progress(nil, 0, false, nil, nil)
    let first = readLastError()
    expect(first != nil, "first read should drain the message")
    let secondPtr = apfs_last_error()
    expect(secondPtr == nil,
           "last_error should clear after read; got non-null pointer")
}

// MARK: - Path containment (audit fix #5)

test("PathContainment: accepts ordinary in-tree relative paths") {
    let resolved = PathContainment.resolveContained(
        scanRoot: "/Users/me",
        relative: "Library/Caches/something"
    )
    expect(resolved == "/Users/me/Library/Caches/something",
           "in-tree path should round-trip; got \(String(describing: resolved))")
}

test("PathContainment: accepts the scan root itself") {
    let resolved = PathContainment.resolveContained(
        scanRoot: "/Users/me",
        relative: ""
    )
    expect(resolved == "/Users/me",
           "empty relative path resolves to the scan root")
}

test("PathContainment: rejects ../ escape from a crafted entry") {
    // The audit's exact attack — a malformed image supplies a
    // relative path whose `..` segments climb out of the scan
    // root. `standardizedFileURL` resolves the `..` lexically;
    // the component prefix check then fails containment.
    let resolved = PathContainment.resolveContained(
        scanRoot: "/Users/me",
        relative: "../../../etc/passwd"
    )
    expect(resolved == nil,
           "../ escape must be refused; got \(String(describing: resolved))")
}

test("PathContainment: rejects deep ../ that lands in /") {
    let resolved = PathContainment.resolveContained(
        scanRoot: "/Users/me",
        relative: "../../"
    )
    expect(resolved == nil,
           "../ to root must be refused; got \(String(describing: resolved))")
}

test("PathContainment: doesn't confuse sibling prefix names") {
    // String-prefix containment would accept this; the
    // component-by-component check refuses it.
    let resolved = PathContainment.resolveContained(
        scanRoot: "/Users/kai",
        relative: ""
    )
    expect(resolved == "/Users/kai", "trivial sanity")
    // Now verify the same root does NOT accept a path that
    // would lexically prefix-match — we construct it via the
    // helper directly with a crafted "relative" that resolves
    // outside, simulating what a malicious entry could try.
    let escape = PathContainment.resolveContained(
        scanRoot: "/Users/kai",
        relative: "../kaiserwilhelm/secrets"
    )
    expect(escape == nil,
           "sibling-prefix path must be refused; got \(String(describing: escape))")
}

test("PathContainment: refuses empty scan root") {
    let resolved = PathContainment.resolveContained(
        scanRoot: "",
        relative: "anything"
    )
    expect(resolved == nil,
           "empty scan root must fail-closed; got \(String(describing: resolved))")
}

test("PathContainment: normalises ./ segments") {
    // Harmless `.` segments — the standardization step erases
    // them and containment passes.
    let resolved = PathContainment.resolveContained(
        scanRoot: "/Users/me",
        relative: "./Documents/./report.txt"
    )
    expect(resolved == "/Users/me/Documents/report.txt",
           "in-tree `.` normalisation should round-trip; got \(String(describing: resolved))")
}

// MARK: - PathContainment Unicode/case (audit #N2 / R2 #6)

test("equalForContainment: NFC == NFC ASCII") {
    let r = PathContainment.equalForContainment("Documents", "Documents",
                                                 caseInsensitive: false)
    expect(r, "ASCII identical strings must match")
}

test("equalForContainment: NFC vs NFD same character matches") {
    // "café" in NFC is `caf\u{00E9}` (precomposed e-acute);
    // in NFD it's `cafe\u{0301}` (e + combining acute). Both
    // refer to the same on-disk filename; without NFC
    // normalisation the `==` rejects a real match.
    let nfc = "caf\u{00E9}"
    let nfd = "cafe\u{0301}"
    let r = PathContainment.equalForContainment(nfc, nfd, caseInsensitive: false)
    expect(r, "NFC/NFD of café must compare equal")
}

test("equalForContainment: case-insensitive folds ASCII") {
    let r = PathContainment.equalForContainment("Documents", "documents",
                                                 caseInsensitive: true)
    expect(r, "case-insensitive ASCII must fold")
}

test("equalForContainment: case-sensitive rejects ASCII case diff") {
    let r = PathContainment.equalForContainment("Documents", "documents",
                                                 caseInsensitive: false)
    expect(!r, "case-sensitive ASCII must not fold")
}

test("equalForContainment: case-insensitive across NFC/NFD + case") {
    // The full combined attack on a case-insensitive volume:
    // entry path is NFD-stored and differently-cased from the
    // root. Pre-fix this would be refused as non-contained.
    let r = PathContainment.equalForContainment("CAFÉ", "cafe\u{0301}",
                                                 caseInsensitive: true)
    expect(r, "NFC+case difference must fold on case-insensitive volume")
}

test("equalForContainment: different strings still reject") {
    let r = PathContainment.equalForContainment("alpha", "beta",
                                                 caseInsensitive: true)
    expect(!r, "genuinely different strings must reject")
}

// MARK: - DisplaySanitizer (audit #App-2 / R2 #4)

test("DisplaySanitizer: pass-through for ordinary names") {
    let s = DisplaySanitizer.sanitiseDisplay("Documents/photo.heic")
    expect(s == "Documents/photo.heic", "got: \(s)")
}

test("DisplaySanitizer: replaces NUL with U+FFFD") {
    let raw = "evil\u{0000}name.txt"
    let s = DisplaySanitizer.sanitiseDisplay(raw)
    expect(s == "evil\u{FFFD}name.txt",
           "NUL must be replaced; got: \(s)")
}

test("DisplaySanitizer: replaces C0 control (backspace)") {
    // U+0008 BACKSPACE would visually 'erase' chars in some
    // terminals if pasted from the menu copy action.
    let raw = "innocent\u{0008}evil"
    let s = DisplaySanitizer.sanitiseDisplay(raw)
    expect(!s.unicodeScalars.contains(where: { $0.value == 0x08 }),
           "C0 control must be replaced; got: \(s)")
}

test("DisplaySanitizer: replaces RTL override (U+202E)") {
    // The classic file-extension-flip attack: visually reads
    // `report.exe` but with the original byte order is
    // `report\u{202E}exe.pdf`. The sanitiser breaks the spoof.
    let raw = "report\u{202E}exe.pdf"
    let s = DisplaySanitizer.sanitiseDisplay(raw)
    expect(!s.unicodeScalars.contains(where: { $0.value == 0x202E }),
           "RTL override must be replaced; got: \(s)")
}

test("DisplaySanitizer: replaces isolate-format (U+2066–2069)") {
    // The post-Unicode-9 evolution of the override-spoof —
    // LEFT-TO-RIGHT ISOLATE and friends.
    let raw = "name\u{2066}\u{2069}.txt"
    let s = DisplaySanitizer.sanitiseDisplay(raw)
    expect(!s.unicodeScalars.contains(where: { (0x2066...0x2069).contains($0.value) }),
           "isolate-format must be replaced; got: \(s)")
}

test("DisplaySanitizer: preserves tab/newline/CR") {
    // Technically C0 but legitimately appear in multi-line
    // filenames on some volumes; passing them through doesn't
    // enable any known spoofing chain.
    let raw = "name\twith\nnewline\r.txt"
    let s = DisplaySanitizer.sanitiseDisplay(raw)
    expect(s == raw, "tab/newline/CR should pass through; got: \(s)")
}

test("DisplaySanitizer: idempotent") {
    // Applying twice gives the same result as applying once —
    // important because the same string can pass through
    // multiple display sites (tooltip, menu, status bar).
    let raw = "evil\u{0000}\u{202E}\u{0008}.txt"
    let once = DisplaySanitizer.sanitiseDisplay(raw)
    let twice = DisplaySanitizer.sanitiseDisplay(once)
    expect(once == twice, "must be idempotent; once=\(once) twice=\(twice)")
}

// MARK: - Wrap up

print("")
if failures == 0 {
    print("apfs-ffi-tests: all \(ran) tests passed")
    exit(0)
} else {
    print("apfs-ffi-tests: \(failures) failure(s) across \(ran) tests")
    exit(1)
}
