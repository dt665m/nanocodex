import Foundation
import XCTest
@testable import InboxCore

final class PublishedOutputDownloadTests: XCTestCase {
    func testAuthenticatedDownloadPreservesVideoExtensionAndBytes() async throws {
        let path = "/brain/outputs/frontiers-next/launch & drop 1.mp4"
        let payload = "private video bytes"
        let fixture = try HTTPFixture { request in
            XCTAssertEqual(request.path, "/v1/agents/agent-1/files")
            XCTAssertEqual(URLComponents(string: "https://fixture.invalid/?" + (request.query ?? ""))?.queryItems?.first?.value, path)
            XCTAssertEqual(request.headers["authorization"], "Bearer " + fixtureKey)
            XCTAssertEqual(request.headers["accept"], "application/octet-stream")
            return FixtureReply(headers: ["Content-Type": "application/octet-stream", "Content-Length": String(payload.utf8.count)], body: payload)
        }
        defer { fixture.close() }
        let client = ManagedClient(credential: try .init(origin: fixture.origin, apiKey: fixtureKey), configuration: fixture.configuration)
        defer { client.close() }
        let file = try await client.downloadOutput(agentID: "agent-1", path: path)
        defer { try? FileManager.default.removeItem(at: file) }
        XCTAssertEqual(file.pathExtension, "mp4")
        XCTAssertEqual(try String(contentsOf: file, encoding: .utf8), payload)
    }

    func testRejectsUnrelatedPrivatePathsBeforeSendingARequest() async throws {
        let fixture = try HTTPFixture { _ in XCTFail("Invalid link caused a network request"); return FixtureReply() }
        defer { fixture.close() }
        let client = ManagedClient(credential: try .init(origin: fixture.origin, apiKey: fixtureKey), configuration: fixture.configuration)
        defer { client.close() }
        for path in ["/brain/tmp/key", "/brain/outputs/../key", "/brain/outputs//file.mp4"] {
            do { _ = try await client.downloadOutput(agentID: "agent-1", path: path); XCTFail(path) }
            catch APIError.invalidResponse { }
        }
    }
}
