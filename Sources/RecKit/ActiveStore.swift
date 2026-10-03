import Foundation

public struct ActiveRecord: Codable, Equatable, Sendable {
    public var pid: Int32
    public var take: String
    /// Wall-clock moment of t0. Stored with milliseconds so `rec mark` and `rec status` can place
    /// "now" on the take clock without talking to the recorder.
    public var startedAt: Date

    public init(pid: Int32, take: String, startedAt: Date) {
        self.pid = pid
        self.take = take
        self.startedAt = startedAt
    }

    public var takeURL: URL { URL(fileURLWithPath: take) }

    public func elapsed(now: Date = Date()) -> Double {
        ((now.timeIntervalSince(startedAt)) * 1000).rounded() / 1000
    }
}

public enum ActiveStore {
    public static var url: URL {
        FileManager.default.homeDirectoryForCurrentUser
            .appendingPathComponent("Library/Application Support/rec/active.json")
    }

    private static let formatter: ISO8601DateFormatter = {
        let f = ISO8601DateFormatter()
        f.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
        return f
    }()

    private static let encoder: JSONEncoder = {
        let e = JSONEncoder()
        e.outputFormatting = [.prettyPrinted, .sortedKeys, .withoutEscapingSlashes]
        e.dateEncodingStrategy = .custom { date, encoder in
            var c = encoder.singleValueContainer()
            try c.encode(formatter.string(from: date))
        }
        return e
    }()

    private static let decoder: JSONDecoder = {
        let d = JSONDecoder()
        d.dateDecodingStrategy = .custom { decoder in
            let c = try decoder.singleValueContainer()
            let s = try c.decode(String.self)
            if let date = formatter.date(from: s) ?? ISO8601DateFormatter().date(from: s) { return date }
            throw DecodingError.dataCorruptedError(in: c, debugDescription: "bad date \(s)")
        }
        return d
    }()

    /// The live record, or nil. A record whose pid is gone is stale and removed on read.
    public static func current() -> ActiveRecord? {
        guard let data = try? Data(contentsOf: url) else { return nil }
        guard let record = try? decoder.decode(ActiveRecord.self, from: data), isAlive(record.pid) else {
            try? FileManager.default.removeItem(at: url)
            return nil
        }
        return record
    }

    public static func write(_ record: ActiveRecord) throws {
        try FileManager.default.createDirectory(at: url.deletingLastPathComponent(), withIntermediateDirectories: true)
        try encoder.encode(record).write(to: url, options: .atomic)
    }

    /// Only the owning recorder clears the record, so a late-exiting recorder never wipes a newer one.
    public static func clear(ownedBy pid: Int32) {
        guard let data = try? Data(contentsOf: url),
              let record = try? decoder.decode(ActiveRecord.self, from: data),
              record.pid == pid else { return }
        try? FileManager.default.removeItem(at: url)
    }

    public static func isAlive(_ pid: Int32) -> Bool {
        kill(pid, 0) == 0 || errno == EPERM
    }
}
