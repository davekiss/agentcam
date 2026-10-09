import Foundation

public struct NormalizedPoint: Equatable, Sendable {
    public var x: Double
    public var y: Double

    public init(x: Double, y: Double) {
        self.x = x
        self.y = y
    }
}

public enum MouseButton: String, Codable, Sendable {
    case left
    case right
    case other
}

public enum TimelineEvent: Equatable, Sendable {
    case cursor(t: Double, at: NormalizedPoint)
    case click(t: Double, at: NormalizedPoint, button: MouseButton)
    case marker(t: Double, label: String)

    public var t: Double {
        switch self {
        case let .cursor(t, _), let .click(t, _, _), let .marker(t, _): return t
        }
    }
}

extension TimelineEvent: Codable {
    private enum Kind: String, Codable {
        case cursor, click, marker
    }

    private enum Key: String, CodingKey {
        case t, type, x, y, button, label
    }

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: Key.self)
        let t = try c.decode(Double.self, forKey: .t)
        switch try c.decode(Kind.self, forKey: .type) {
        case .cursor:
            self = .cursor(t: t, at: NormalizedPoint(x: try c.decode(Double.self, forKey: .x), y: try c.decode(Double.self, forKey: .y)))
        case .click:
            self = .click(
                t: t,
                at: NormalizedPoint(x: try c.decode(Double.self, forKey: .x), y: try c.decode(Double.self, forKey: .y)),
                button: try c.decode(MouseButton.self, forKey: .button)
            )
        case .marker:
            self = .marker(t: t, label: try c.decode(String.self, forKey: .label))
        }
    }

    public func encode(to encoder: Encoder) throws {
        var c = encoder.container(keyedBy: Key.self)
        try c.encode(Self.round(t), forKey: .t)
        switch self {
        case let .cursor(_, p):
            try c.encode(Kind.cursor, forKey: .type)
            try c.encode(Self.round(p.x), forKey: .x)
            try c.encode(Self.round(p.y), forKey: .y)
        case let .click(_, p, button):
            try c.encode(Kind.click, forKey: .type)
            try c.encode(Self.round(p.x), forKey: .x)
            try c.encode(Self.round(p.y), forKey: .y)
            try c.encode(button, forKey: .button)
        case let .marker(_, label):
            try c.encode(Kind.marker, forKey: .type)
            try c.encode(label, forKey: .label)
        }
    }

    private static func round(_ v: Double) -> Double {
        (v * 1000).rounded() / 1000
    }
}

public struct Timeline: Codable, Equatable, Sendable {
    public var version: Int = 1
    public var events: [TimelineEvent]

    public init(events: [TimelineEvent]) {
        self.events = events.sorted { $0.t < $1.t }
    }

    /// Mean cursor position over the take, used to center the 9:16 crop.
    public var meanCursor: NormalizedPoint? {
        let points: [NormalizedPoint] = events.compactMap {
            if case let .cursor(_, p) = $0 { return p }
            return nil
        }
        guard !points.isEmpty else { return nil }
        let n = Double(points.count)
        return NormalizedPoint(x: points.map(\.x).reduce(0, +) / n, y: points.map(\.y).reduce(0, +) / n)
    }
}

/// One line of markers.jsonl, appended by `agentcam-mac mark` while recording. `t` is on the take clock.
public struct MarkerLine: Codable, Equatable, Sendable {
    public var t: Double
    public var label: String

    public init(t: Double, label: String) {
        self.t = t
        self.label = label
    }

    public static func parse(jsonl: String) -> [MarkerLine] {
        jsonl.split(separator: "\n").compactMap { try? JSONDecoder().decode(MarkerLine.self, from: Data($0.utf8)) }
    }
}
