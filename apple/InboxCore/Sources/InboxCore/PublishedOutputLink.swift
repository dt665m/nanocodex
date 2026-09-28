import Foundation

/// A link to a generated file in this conversation's private Brain workspace.
/// Never treat a sandbox URI as a public URL or accept an arbitrary Hand path.
public struct PublishedOutputLink: Identifiable, Equatable, Hashable, Sendable {
    public let path: String
    public let title: String
    public var id: String { path }
    public var filename: String { String(path.split(separator: "/").last ?? "output") }
    public var fileExtension: String { (filename as NSString).pathExtension.lowercased() }
    public var isVideo: Bool { ["mp4", "mov", "m4v", "webm"].contains(fileExtension) }
    public var isImage: Bool { ["png", "jpg", "jpeg", "gif", "webp", "heic"].contains(fileExtension) }

    public init?(url: URL, title: String = "") {
        guard url.scheme?.lowercased() == "sandbox", url.host == nil, url.user == nil,
              url.password == nil, url.query == nil, url.fragment == nil,
              let components = URLComponents(url: url, resolvingAgainstBaseURL: false),
              let decoded = components.percentEncodedPath.removingPercentEncoding,
              Self.validPath(decoded) else { return nil }
        path = decoded
        let label = title.trimmingCharacters(in: .whitespacesAndNewlines)
        self.title = label.isEmpty ? String(decoded.split(separator: "/").last!) : String(label.prefix(160))
    }

    public static func validPath(_ path: String) -> Bool {
        path.hasPrefix("/brain/outputs/") && path.utf8.count <= 8192
            && !path.unicodeScalars.contains(where: { $0.value < 32 || $0.value == 127 || $0 == "\\" })
            && path.split(separator: "/", omittingEmptySubsequences: false).dropFirst().allSatisfy { !$0.isEmpty && $0 != "." && $0 != ".." }
    }

    /// Only assistant-authored Markdown links become cards. Plain text, quoted
    /// paths, user messages and untrusted tool output do not trigger downloads.
    public static func parse(_ markdown: String) -> [Self] {
        guard let regex = try? NSRegularExpression(pattern: #"\[([^\]\n]{1,160})\]\((?:<(sandbox:/brain/outputs/[^>\n]+)>|(sandbox:/brain/outputs/[^\s)<>]+))\)"#) else { return [] }
        let source = markdown as NSString
        var result: [Self] = [], seen = Set<String>()
        for match in regex.matches(in: markdown, range: NSRange(location: 0, length: source.length)).prefix(64) {
            let range = match.range(at: match.range(at: 2).location == NSNotFound ? 3 : 2)
            guard let url = URL(string: source.substring(with: range)),
                  let link = Self(url: url, title: source.substring(with: match.range(at: 1))),
                  seen.insert(link.path).inserted else { continue }
            result.append(link)
        }
        return result
    }
}
