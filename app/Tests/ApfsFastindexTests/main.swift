import Foundation
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

// MARK: - Wrap up

print("")
if failures == 0 {
    print("apfs-ffi-tests: all \(ran) tests passed")
    exit(0)
} else {
    print("apfs-ffi-tests: \(failures) failure(s) across \(ran) tests")
    exit(1)
}
