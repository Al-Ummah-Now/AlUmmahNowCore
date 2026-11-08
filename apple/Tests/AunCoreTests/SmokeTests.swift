import XCTest
@testable import AunCore

/// Proves sync calls, async calls, errors and callbacks cross the FFI boundary from Swift.
final class SmokeTests: XCTestCase {
    func testVersion() {
        XCTAssertFalse(coreVersion().isEmpty)
    }

    func testAsyncEcho() async {
        let out = await selftestAsyncEcho(message: "سلام", delayMs: 5)
        XCTAssertEqual(out, String("سلام".reversed()))
    }

    func testErrorsMapToAunError() {
        XCTAssertThrowsError(try selftestThrow(kind: "rate_limited")) { error in
            guard case AunError.RateLimited(let retryAfterMs) = error else {
                return XCTFail("unexpected \(error)")
            }
            XCTAssertEqual(retryAfterMs, 1500)
        }
    }

    final class Listener: SelfTestListener, @unchecked Sendable {
        private let lock = NSLock()
        private(set) var indices: [UInt32] = []
        func onEvent(index: UInt32, message: String) {
            lock.lock(); indices.append(index); lock.unlock()
        }
    }

    func testCallbacks() async {
        let listener = Listener()
        let n = await selftestCallbacks(listener: listener, count: 4)
        XCTAssertEqual(n, 4)
        XCTAssertEqual(listener.indices, [0, 1, 2, 3])
    }

    final class MemoryStorage: SecureStorage, @unchecked Sendable {
        private let lock = NSLock()
        private var items: [String: Data] = [:]
        func get(key: String) throws -> Data? { lock.lock(); defer { lock.unlock() }; return items[key] }
        func set(key: String, value: Data) throws { lock.lock(); items[key] = value; lock.unlock() }
        func delete(key: String) throws { lock.lock(); items[key] = nil; lock.unlock() }
        func list(prefix: String) throws -> [String] {
            lock.lock(); defer { lock.unlock() }; return items.keys.filter { $0.hasPrefix(prefix) }.sorted()
        }
    }

    func testSecureStorageAdapter() throws {
        let bytes = try selftestSecureStorage(storage: MemoryStorage())
        XCTAssertEqual(String(data: bytes, encoding: .utf8), "سلام")
    }

    final class Offline: Connectivity, @unchecked Sendable {
        func current() -> NetworkState { NetworkState(online: false, metered: true) }
    }

    func testConnectivityAdapter() {
        XCTAssertEqual(selftestConnectivity(connectivity: Offline()), NetworkState(online: false, metered: true))
    }

    func testApiClientSurface() async throws {
        XCTAssertTrue(apiOperations().contains { $0.path == "/api/videos/upload/initiate" })
        let client = try ApiClient(
            config: ApiClientConfig(baseUrl: "http://127.0.0.1:9", userAgent: "smoke", locale: "ar", timeoutMs: 2000, maxAttempts: 1),
            tokens: nil
        )
        do {
            _ = try await client.request(call: ApiCall(operationId: "get_GetI18nConfigEndpoint", pathParams: [:], query: [:], headers: [:], bodyJson: nil, idempotencyKey: nil))
            XCTFail("expected an error")
        } catch AunError.Network {
        } catch AunError.InvalidInput {
        }
    }

    func testAuthManagerSurface() async throws {
        let auth = try AuthManager(
            config: AuthConfig(issuer: "https://auth.example.invalid", clientId: "viewer-mobile",
                               redirectUri: "alummahnow://auth/callback", scopes: ["openid"],
                               authorizationEndpoint: nil, tokenEndpoint: nil, deviceAuthorizationEndpoint: nil),
            storage: MemoryStorage()
        )
        let req = try auth.beginLogin()
        XCTAssertTrue(req.url.contains("code_challenge_method=S256"))
        XCTAssertTrue(try auth.accounts().isEmpty)
        let token = try await auth.accessToken()
        XCTAssertNil(token)
        do {
            _ = try await auth.beginDeviceLogin()
            XCTFail("expected Unsupported")
        } catch AunError.Unsupported {
        }
    }

    func testUploadEngineSurface() throws {
        let client = try ApiClient(
            config: ApiClientConfig(baseUrl: "http://127.0.0.1:9", userAgent: "smoke", locale: nil, timeoutMs: 2000, maxAttempts: 1),
            tokens: nil
        )
        let dir = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString).path
        let config = uploadConfigDefault()
        XCTAssertEqual(config.maxParallel, 3)
        let engine = try UploadEngine(client: client, storeDir: dir, accountId: "u1", config: config, connectivity: nil)
        XCTAssertTrue(try engine.list().isEmpty)
        let request = UploadRequest(filePath: "/nonexistent/clip.mp4", title: "درس", description: nil, mimeType: nil,
                                    channelId: nil, isPublic: nil, tags: nil, category: nil,
                                    titleLocalized: [:], descriptionLocalized: [:], priority: 0)
        XCTAssertThrowsError(try engine.enqueue(request: request)) { error in
            guard case AunError.InvalidInput = error else { return XCTFail("\(error)") }
        }
        engine.setBandwidthLimit(bytesPerSec: 1_000_000)
        engine.shutdown()
    }
}
