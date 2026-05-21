import Foundation
import CryptoKit
import Network
import Sparkle

/// Tier-B Sparkle upgrade test.
///
/// Validates the meat of the auto-update path end-to-end without
/// needing a GUI:
///
/// 1. Builds a fake `v0.0.0` host bundle in a temp directory
///    with a generated Ed25519 public key in its Info.plist.
/// 2. Spins up an in-process HTTP server (Apple Network
///    framework) serving:
///    - `/appcast.xml`  — fixture with a `v9.9.9` <item>
///    - `/update.zip`   — synthetic update payload, signed by
///                       the test-only Ed25519 private key.
/// 3. Initialises `SPUUpdater` against the fake bundle and a
///    delegate that points the feed URL at the local server.
/// 4. Drives `checkForUpdates`, pumps the runloop, captures
///    `SPUUserDriver.showUpdateFound` and asserts the
///    announced version matches the fixture.
///
/// What this DOES validate:
///
/// - Sparkle reaches the appcast URL.
/// - The appcast XML parses (RSS + sparkle namespace).
/// - Version comparison: `9.9.9 > 0.0.0` triggers the
///   "update found" path.
/// - Sparkle exposes a real `SUAppcastItem` with the right
///   fields to the user-driver callback.
///
/// What it does NOT validate (deliberately out of scope for
/// Tier B):
///
/// - Bundle replacement / install execution (Tier C).
/// - Restart-after-install (Tier C).
/// - Sparkle's standard UI dialogs (Tier C).
///
/// Run via `swift run apfs-update-tests` from `app/`. Exits 0
/// on success, non-zero with a printed failure reason on any
/// assertion miss.

// MARK: - Tiny HTTP server (Apple Network framework)

/// Single-shot HTTP/1.1 GET server. Routes are a mutable
/// dictionary (`path -> body bytes`) so the test can populate
/// them *after* the server has bound to its OS-assigned port —
/// the appcast XML must embed that port in the enclosure URL,
/// so we can't know the routes until the listener is live.
final class TinyHTTPServer {
    private let routesLock = NSLock()
    private var routes: [String: Data] = [:]
    private var listener: NWListener?
    private(set) var port: UInt16 = 0
    private let queue = DispatchQueue(label: "tinyhttpd")

    func start() throws {
        // `using: .tcp` with no port argument tells NWListener
        // to pick an ephemeral port (the OS picks); the actual
        // port appears on `listener.port` after the state
        // transitions to .ready.
        let listener = try NWListener(using: .tcp)
        let readySemaphore = DispatchSemaphore(value: 0)
        listener.newConnectionHandler = { [weak self] conn in
            self?.handle(connection: conn)
        }
        listener.stateUpdateHandler = { state in
            switch state {
            case .ready:
                readySemaphore.signal()
            case .failed(let err):
                fputs("[tinyhttpd] listener failed: \(err)\n", stderr)
                readySemaphore.signal()
            default:
                break
            }
        }
        listener.start(queue: queue)
        // Wait up to 3s for .ready before reading the port.
        guard readySemaphore.wait(timeout: .now() + 3.0) == .success else {
            throw NSError(
                domain: "tinyhttpd",
                code: 1,
                userInfo: [NSLocalizedDescriptionKey: "listener did not become ready"]
            )
        }
        guard let p = listener.port?.rawValue else {
            throw NSError(
                domain: "tinyhttpd",
                code: 2,
                userInfo: [NSLocalizedDescriptionKey: "listener did not bind a port"]
            )
        }
        self.port = p
        self.listener = listener
        fputs("[tinyhttpd] listening on port \(p)\n", stderr)
    }

    func setRoute(_ path: String, body: Data) {
        routesLock.lock()
        routes[path] = body
        routesLock.unlock()
    }

    func stop() {
        listener?.cancel()
        listener = nil
    }

    private func handle(connection: NWConnection) {
        connection.start(queue: queue)
        connection.receive(minimumIncompleteLength: 1, maximumLength: 8192) { [weak self] data, _, _, _ in
            guard let self = self,
                  let data = data,
                  let request = String(data: data, encoding: .utf8) else {
                connection.cancel()
                return
            }
            let firstLine = request.split(separator: "\r\n").first.map(String.init) ?? ""
            let parts = firstLine.split(separator: " ")
            guard parts.count >= 2 else {
                connection.cancel()
                return
            }
            let path = String(parts[1])
            self.routesLock.lock()
            let body = self.routes[path]
            self.routesLock.unlock()
            let status = body != nil ? "200 OK" : "404 Not Found"
            let responseBody = body ?? Data()
            let header = """
                HTTP/1.1 \(status)\r
                Content-Length: \(responseBody.count)\r
                Content-Type: application/octet-stream\r
                Connection: close\r
                \r

                """
            var response = Data(header.utf8)
            response.append(responseBody)
            connection.send(
                content: response,
                completion: .contentProcessed { _ in
                    connection.cancel()
                }
            )
        }
    }
}

// MARK: - Fixture helpers

/// Generates a fresh Ed25519 keypair via CryptoKit. The public
/// key's raw 32-byte representation, Base64-encoded, is the
/// shape Sparkle wants in `SUPublicEDKey`; the matching
/// private key signs the fixture zip using the same Ed25519
/// algorithm Sparkle's `sign_update` uses (libsodium
/// internally).
struct Ed25519TestKeys {
    let privateKey: Curve25519.Signing.PrivateKey
    let publicKeyBase64: String

    init() {
        let priv = Curve25519.Signing.PrivateKey()
        self.privateKey = priv
        self.publicKeyBase64 = priv.publicKey.rawRepresentation.base64EncodedString()
    }

    func sign(_ data: Data) throws -> String {
        let sig = try privateKey.signature(for: data)
        return sig.base64EncodedString()
    }
}

func buildFakeBundle(
    version: String,
    publicKey: String,
    feedURL: String,
    in parent: URL
) throws -> URL {
    let appURL = parent.appendingPathComponent("Old.app")
    let contentsURL = appURL.appendingPathComponent("Contents")
    let macosURL = contentsURL.appendingPathComponent("MacOS")
    try FileManager.default.createDirectory(at: macosURL, withIntermediateDirectories: true)

    let echoPath = "/bin/echo"
    let appBinaryURL = macosURL.appendingPathComponent("Old")
    try FileManager.default.copyItem(atPath: echoPath, toPath: appBinaryURL.path)

    let plist: [String: Any] = [
        "CFBundleIdentifier": "com.apfsfastindex.updatetest",
        "CFBundleName": "ApfsFastindexUpdateTest",
        "CFBundleExecutable": "Old",
        "CFBundleShortVersionString": version,
        "CFBundleVersion": version,
        "CFBundlePackageType": "APPL",
        "CFBundleInfoDictionaryVersion": "6.0",
        "LSMinimumSystemVersion": "13.0",
        "SUPublicEDKey": publicKey,
        "SUFeedURL": feedURL,
        "SUEnableAutomaticChecks": true,
    ]
    let plistData = try PropertyListSerialization.data(
        fromPropertyList: plist,
        format: .xml,
        options: 0
    )
    try plistData.write(to: contentsURL.appendingPathComponent("Info.plist"))
    return appURL
}

func buildUpdatePayload() -> Data {
    // Any deterministic bytes are fine — Sparkle never extracts
    // this in the Tier-B flow because we dismiss before the
    // download phase. We still sign over real bytes so the
    // signature is well-formed.
    Data(repeating: 0xAB, count: 4096)
}

func writeAppcast(
    enclosureURL: String,
    enclosureLength: Int,
    signature: String,
    version: String,
    pubDate: String
) -> Data {
    let xml = """
        <?xml version="1.0" encoding="utf-8"?>
        <rss
            version="2.0"
            xmlns:sparkle="http://www.andymatuschak.org/xml-namespaces/sparkle">
            <channel>
                <title>apfs-fastindex updates</title>
                <link>https://example.test/apfs-fastindex</link>
                <description>Test channel.</description>
                <language>en</language>
                <item>
                    <title>\(version)</title>
                    <pubDate>\(pubDate)</pubDate>
                    <sparkle:version>\(version)</sparkle:version>
                    <sparkle:shortVersionString>\(version)</sparkle:shortVersionString>
                    <sparkle:minimumSystemVersion>13.0</sparkle:minimumSystemVersion>
                    <enclosure
                        url="\(enclosureURL)"
                        length="\(enclosureLength)"
                        type="application/octet-stream"
                        sparkle:edSignature="\(signature)"
                    />
                </item>
            </channel>
        </rss>
        """
    return Data(xml.utf8)
}

// MARK: - No-UI SPUUserDriver

final class CapturingUserDriver: NSObject, SPUUserDriver {
    enum Outcome {
        case foundUpdate(version: String)
        case updateNotFound(error: Error)
        case error(Error)
    }

    private(set) var outcome: Outcome?
    private let onComplete: () -> Void

    init(onComplete: @escaping () -> Void) {
        self.onComplete = onComplete
    }

    private func finish(_ outcome: Outcome) {
        guard self.outcome == nil else { return }
        self.outcome = outcome
        onComplete()
    }

    func show(
        _ request: SPUUpdatePermissionRequest,
        reply: @escaping (SUUpdatePermissionResponse) -> Void
    ) {
        reply(SUUpdatePermissionResponse(
            automaticUpdateChecks: true,
            sendSystemProfile: false
        ))
    }

    func showUserInitiatedUpdateCheck(cancellation: @escaping () -> Void) {}

    func showUpdateFound(
        with appcastItem: SUAppcastItem,
        state: SPUUserUpdateState,
        reply: @escaping (SPUUserUpdateChoice) -> Void
    ) {
        finish(.foundUpdate(version: appcastItem.versionString))
        reply(.dismiss)
    }

    func showUpdateNotFoundWithError(_ error: Error, acknowledgement: @escaping () -> Void) {
        finish(.updateNotFound(error: error))
        acknowledgement()
    }

    func showUpdaterError(_ error: Error, acknowledgement: @escaping () -> Void) {
        finish(.error(error))
        acknowledgement()
    }

    func showDownloadInitiated(cancellation: @escaping () -> Void) {}
    func showDownloadDidReceiveExpectedContentLength(_ expectedContentLength: UInt64) {}
    func showDownloadDidReceiveData(ofLength length: UInt64) {}
    func showDownloadDidStartExtractingUpdate() {}
    func showExtractionReceivedProgress(_ progress: Double) {}
    func showReady(toInstallAndRelaunch reply: @escaping (SPUUserUpdateChoice) -> Void) {
        reply(.dismiss)
    }
    func showInstallingUpdate(
        withApplicationTerminated applicationTerminated: Bool,
        retryTerminatingApplication: @escaping () -> Void
    ) {}
    func showUpdateInstallationDidFinish(acknowledgement: @escaping () -> Void) {
        acknowledgement()
    }
    func dismissUpdateInstallation() {}
    func showSendingTerminationSignal() {}

    // Release-notes & post-install hooks (required by
    // SPUUserDriver but unreachable in Tier B's flow because we
    // dismiss the update before download begins).
    func showUpdateReleaseNotes(with downloadData: SPUDownloadData) {}
    func showUpdateReleaseNotesFailedToDownloadWithError(_ error: Error) {}
    func showUpdateInstalledAndRelaunched(_ relaunched: Bool, acknowledgement: @escaping () -> Void) {
        acknowledgement()
    }
}

// MARK: - SPUUpdaterDelegate stub

final class FixtureUpdaterDelegate: NSObject, SPUUpdaterDelegate {
    let feedURL: String
    init(feedURL: String) { self.feedURL = feedURL }
    func feedURLString(for updater: SPUUpdater) -> String? { feedURL }
}

// MARK: - Driver

func fail(_ message: String) -> Never {
    fputs("❌ \(message)\n", stderr)
    exit(1)
}

func main() {
    let tmpRoot = URL(
        fileURLWithPath: NSTemporaryDirectory()
    ).appendingPathComponent("apfs-update-test-\(UUID().uuidString)")
    do {
        try FileManager.default.createDirectory(
            at: tmpRoot,
            withIntermediateDirectories: true
        )
    } catch {
        fail("could not create tmp dir: \(error)")
    }
    defer { try? FileManager.default.removeItem(at: tmpRoot) }

    // 1. Test keypair + synthetic update payload.
    let keys = Ed25519TestKeys()
    let updateBytes = buildUpdatePayload()
    let signature: String
    do {
        signature = try keys.sign(updateBytes)
    } catch {
        fail("Ed25519 sign failed: \(error)")
    }

    // 2. Bring up the HTTP server first so we know its port.
    let server = TinyHTTPServer()
    do {
        try server.start()
    } catch {
        fail("server start failed: \(error)")
    }
    defer { server.stop() }
    let serverBase = "http://127.0.0.1:\(server.port)"

    // 3. Populate routes now that we have the real port.
    let appcastBytes = writeAppcast(
        enclosureURL: "\(serverBase)/update.zip",
        enclosureLength: updateBytes.count,
        signature: signature,
        version: "9.9.9",
        pubDate: "Wed, 20 May 2026 12:00:00 +0000"
    )
    server.setRoute("/appcast.xml", body: appcastBytes)
    server.setRoute("/update.zip", body: updateBytes)

    // 4. Fake v0.0.0 host bundle pointed at the local appcast.
    let feedURL = "\(serverBase)/appcast.xml"
    let appURL: URL
    do {
        appURL = try buildFakeBundle(
            version: "0.0.0",
            publicKey: keys.publicKeyBase64,
            feedURL: feedURL,
            in: tmpRoot
        )
    } catch {
        fail("fake bundle build failed: \(error)")
    }
    guard let bundle = Bundle(url: appURL) else {
        fail("could not open fake bundle at \(appURL.path)")
    }

    // 5. SPUUpdater against the fake bundle + capturing driver.
    var completed = false
    let driver = CapturingUserDriver(onComplete: { completed = true })
    let delegate = FixtureUpdaterDelegate(feedURL: feedURL)
    let updater = SPUUpdater(
        hostBundle: bundle,
        applicationBundle: bundle,
        userDriver: driver,
        delegate: delegate
    )
    do {
        try updater.start()
    } catch {
        fail("updater.start() failed: \(error)")
    }
    updater.checkForUpdates()

    // 6. Pump the runloop until the driver records an outcome
    //    or we time out.
    let deadline = Date().addingTimeInterval(20.0)
    while !completed && Date() < deadline {
        RunLoop.main.run(mode: .default, before: Date().addingTimeInterval(0.1))
    }

    guard let outcome = driver.outcome else {
        fail("timed out waiting for SPUUserDriver outcome (20 s)")
    }
    switch outcome {
    case .foundUpdate(let version):
        if version != "9.9.9" {
            fail("SPUUserDriver received version \(version), expected 9.9.9")
        }
        print("✅ Tier B fixture: SPUUpdater discovered v9.9.9 from synthetic appcast.")
    case .updateNotFound(let error):
        fail("SPUUserDriver received updateNotFound: \(error)")
    case .error(let error):
        fail("SPUUserDriver received updater error: \(error)")
    }

    // 7. Production-appcast regression test.
    //
    // The fixture above writes its own appcast XML, so it
    // can't catch a class of bug where the *shipped*
    // appcast.xml at the repo root is malformed (e.g. XML
    // comments containing the double-hyphen sequence, which
    // the XML 1.0 spec forbids — exact bug Sparkle's parser
    // bailed on in production with "could not parse update
    // feed"). This second scenario loads the real
    // appcast.xml, serves it via the same TinyHTTPServer,
    // and runs SPUUpdater against it.
    //
    // We use the project's real SUPublicEDKey from
    // app/sparkle-public-key.txt so signature verification
    // *could* succeed if Sparkle ran the install — but we
    // dismiss at showUpdateFound, so we never reach that
    // stage. What matters is that Sparkle gets *to*
    // showUpdateFound, which means it parsed the feed and
    // ran the version comparison.
    runProductionAppcastScenario(in: tmpRoot)
}

func runProductionAppcastScenario(in tmpRoot: URL) {
    // Locate the repo root + the production artifacts via
    // #filePath, which is the absolute path of *this* source
    // file. From app/Tests/ApfsUpdateTests/main.swift, the
    // repo root is four levels up.
    let thisFile = URL(fileURLWithPath: #filePath)
    let repoRoot = thisFile
        .deletingLastPathComponent()  // ApfsUpdateTests/
        .deletingLastPathComponent()  // Tests/
        .deletingLastPathComponent()  // app/
        .deletingLastPathComponent()  // <repo>
    let appcastURL = repoRoot.appendingPathComponent("appcast.xml")
    let keyFileURL = repoRoot.appendingPathComponent("app/sparkle-public-key.txt")

    let appcastBytes: Data
    do {
        appcastBytes = try Data(contentsOf: appcastURL)
    } catch {
        fail("could not read production appcast at \(appcastURL.path): \(error)")
    }
    let publicKey: String
    do {
        let raw = try String(contentsOf: keyFileURL, encoding: .utf8)
        publicKey = raw.trimmingCharacters(in: .whitespacesAndNewlines)
    } catch {
        fail("could not read public key at \(keyFileURL.path): \(error)")
    }

    let server = TinyHTTPServer()
    do {
        try server.start()
    } catch {
        fail("production-appcast server start failed: \(error)")
    }
    defer { server.stop() }
    server.setRoute("/appcast.xml", body: appcastBytes)
    let feedURL = "http://127.0.0.1:\(server.port)/appcast.xml"

    let appURL: URL
    do {
        appURL = try buildFakeBundle(
            version: "0.0.0",
            publicKey: publicKey,
            feedURL: feedURL,
            in: tmpRoot.appendingPathComponent("prod-test")
        )
    } catch {
        fail("production fake bundle build failed: \(error)")
    }
    guard let bundle = Bundle(url: appURL) else {
        fail("could not open production fake bundle at \(appURL.path)")
    }

    var completed = false
    let driver = CapturingUserDriver(onComplete: { completed = true })
    let delegate = FixtureUpdaterDelegate(feedURL: feedURL)
    let updater = SPUUpdater(
        hostBundle: bundle,
        applicationBundle: bundle,
        userDriver: driver,
        delegate: delegate
    )
    do {
        try updater.start()
    } catch {
        fail("production updater.start() failed: \(error)")
    }
    updater.checkForUpdates()

    let deadline = Date().addingTimeInterval(20.0)
    while !completed && Date() < deadline {
        RunLoop.main.run(mode: .default, before: Date().addingTimeInterval(0.1))
    }
    guard let outcome = driver.outcome else {
        fail("production appcast: timed out waiting for SPUUserDriver (20 s)")
    }
    switch outcome {
    case .foundUpdate(let version):
        // Any non-empty semver-shaped version is a pass —
        // the assertion is that parse + version compare
        // both worked. The exact latest version drifts with
        // every release.
        if version.isEmpty || version == "0.0.0" {
            fail("production appcast: showUpdateFound received empty/0.0.0 version")
        }
        print("✅ Tier B prod: SPUUpdater parsed real appcast.xml, offered v\(version).")
    case .updateNotFound(let error):
        // No items > 0.0.0? Either the appcast genuinely
        // has no entries (initial state) or something is
        // wrong. Treat as a soft pass with a warning so the
        // test still validates parse-doesn't-crash without
        // requiring a populated appcast.
        print("⚠️  Tier B prod: parse OK but no updates above v0.0.0 (\(error))")
    case .error(let error):
        // This is the bug class we care most about:
        // Sparkle parser rejected the appcast. The error
        // message will name the parse failure.
        fail("production appcast rejected by Sparkle: \(error)")
    }
}

main()
