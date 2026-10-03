import Foundation

public struct Rect: Codable, Equatable, Sendable {
    public var x: Double
    public var y: Double
    public var width: Double
    public var height: Double

    public init(x: Double, y: Double, width: Double, height: Double) {
        self.x = x
        self.y = y
        self.width = width
        self.height = height
    }

    public var cgRect: CGRect { CGRect(x: x, y: y, width: width, height: height) }
}

public struct Source: Codable, Equatable, Sendable {
    public enum Kind: String, Codable, Sendable {
        case display
        case window
    }

    public var kind: Kind
    public var id: UInt32
    public var title: String
    public var app: String?
    public var frame: Rect
    public var scale: Double

    public init(kind: Kind, id: UInt32, title: String, app: String?, frame: Rect, scale: Double) {
        self.kind = kind
        self.id = id
        self.title = title
        self.app = app
        self.frame = frame
        self.scale = scale
    }
}

public struct Track: Codable, Equatable, Sendable {
    public enum Kind: String, Codable, Sendable, CaseIterable {
        case screen
        case camera
        case mic

        public var file: String {
            switch self {
            case .screen: return "screen.mov"
            case .camera: return "cam.mov"
            case .mic: return "mic.m4a"
            }
        }
    }

    public var kind: Kind
    public var file: String
    public var offset: Double
    public var width: Int?
    public var height: Int?

    public init(kind: Kind, offset: Double, width: Int? = nil, height: Int? = nil) {
        self.kind = kind
        self.file = kind.file
        self.offset = offset
        self.width = width
        self.height = height
    }
}

public struct Take: Codable, Equatable, Sendable {
    public enum Status: String, Codable, Sendable {
        case recording
        case finished
        case failed
    }

    public var version: Int = 1
    public var id: String
    public var createdAt: Date
    public var status: Status
    public var duration: Double?
    public var source: Source
    public var tracks: [Track]
    /// Present only when status is failed, so an agent can see why without reading recorder.log.
    public var error: CommandError?

    public init(id: String, createdAt: Date, status: Status, duration: Double? = nil, source: Source, tracks: [Track] = [], error: CommandError? = nil) {
        self.id = id
        self.createdAt = createdAt
        self.status = status
        self.duration = duration
        self.source = source
        self.tracks = tracks
        self.error = error
    }

    public func track(_ kind: Track.Kind) -> Track? {
        tracks.first { $0.kind == kind }
    }
}

public enum TakeFile {
    public static let manifest = "take.json"
    public static let timeline = "timeline.json"
    public static let markers = "markers.jsonl"
    public static let log = "recorder.log"
}

public enum JSON {
    public static let encoder: JSONEncoder = {
        let e = JSONEncoder()
        e.outputFormatting = [.prettyPrinted, .sortedKeys, .withoutEscapingSlashes]
        e.dateEncodingStrategy = .iso8601
        return e
    }()

    public static let decoder: JSONDecoder = {
        let d = JSONDecoder()
        d.dateDecodingStrategy = .iso8601
        return d
    }()

    public static func write<T: Encodable>(_ value: T, to url: URL) throws {
        let data = try encoder.encode(value)
        try data.write(to: url, options: .atomic)
    }

    public static func read<T: Decodable>(_ type: T.Type, from url: URL) throws -> T {
        try decoder.decode(type, from: Data(contentsOf: url))
    }
}

public enum TakeID {
    public static func make(now: Date = Date()) -> String {
        let f = DateFormatter()
        f.locale = Locale(identifier: "en_US_POSIX")
        f.dateFormat = "yyyyMMdd-HHmmss"
        return "take-" + f.string(from: now)
    }
}
