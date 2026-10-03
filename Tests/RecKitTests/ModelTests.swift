import Foundation
import XCTest
@testable import RecKit

final class ModelTests: XCTestCase {
    func testTakeDecodesSpecExample() throws {
        let json = """
        {
          "version": 1,
          "id": "take-20261002-213501",
          "createdAt": "2026-10-02T21:35:01Z",
          "status": "finished",
          "duration": 42.7,
          "source": {
            "kind": "window",
            "id": 1,
            "title": "Built-in Retina Display",
            "app": "Google Chrome",
            "frame": { "x": 0, "y": 0, "width": 1512, "height": 982 },
            "scale": 2
          },
          "tracks": [
            { "kind": "screen", "file": "screen.mov", "offset": 0.0,  "width": 3024, "height": 1964 },
            { "kind": "camera", "file": "cam.mov",    "offset": 0.12, "width": 1920, "height": 1080 },
            { "kind": "mic",    "file": "mic.m4a",    "offset": 0.03 }
          ]
        }
        """
        let take = try JSON.decoder.decode(Take.self, from: Data(json.utf8))
        XCTAssertEqual(take.status, .finished)
        XCTAssertEqual(take.source.kind, .window)
        XCTAssertEqual(take.source.frame, Rect(x: 0, y: 0, width: 1512, height: 982))
        XCTAssertEqual(take.track(.camera)?.offset, 0.12)
        XCTAssertEqual(take.track(.camera)?.file, "cam.mov")
        XCTAssertNil(take.track(.mic)?.width)
        XCTAssertEqual(take.createdAt, ISO8601DateFormatter().date(from: "2026-10-02T21:35:01Z"))
    }

    func testTakeEncodesSpecFieldNames() throws {
        let take = Take(
            id: "take-1",
            createdAt: Date(timeIntervalSince1970: 0),
            status: .recording,
            source: Source(kind: .display, id: 1, title: "Display", app: nil, frame: Rect(x: 0, y: 0, width: 10, height: 10), scale: 2),
            tracks: [Track(kind: .mic, offset: 0.03)]
        )
        let object = try JSONSerialization.jsonObject(with: JSON.encoder.encode(take)) as! [String: Any]
        XCTAssertEqual(Set(object.keys), ["version", "id", "createdAt", "status", "source", "tracks"])
        XCTAssertEqual(object["status"] as? String, "recording")
        XCTAssertEqual(object["createdAt"] as? String, "1970-01-01T00:00:00Z")
        let track = (object["tracks"] as! [[String: Any]])[0]
        XCTAssertEqual(Set(track.keys), ["kind", "file", "offset"])
        XCTAssertEqual(track["file"] as? String, "mic.m4a")
        XCTAssertEqual(try JSON.decoder.decode(Take.self, from: JSON.encoder.encode(take)), take)
    }

    func testTimelineRoundtripsFlatEvents() throws {
        let json = """
        {
          "version": 1,
          "events": [
            { "t": 0.033, "type": "cursor", "x": 0.412, "y": 0.230 },
            { "t": 1.200, "type": "click",  "x": 0.415, "y": 0.231, "button": "left" },
            { "t": 3.900, "type": "marker", "label": "ran tests" }
          ]
        }
        """
        let timeline = try JSON.decoder.decode(Timeline.self, from: Data(json.utf8))
        XCTAssertEqual(timeline.events, [
            .cursor(t: 0.033, at: NormalizedPoint(x: 0.412, y: 0.230)),
            .click(t: 1.2, at: NormalizedPoint(x: 0.415, y: 0.231), button: .left),
            .marker(t: 3.9, label: "ran tests"),
        ])

        let encoded = try JSONSerialization.jsonObject(with: JSON.encoder.encode(timeline)) as! [String: Any]
        let events = encoded["events"] as! [[String: Any]]
        XCTAssertEqual(Set(events[0].keys), ["t", "type", "x", "y"])
        XCTAssertEqual(Set(events[1].keys), ["t", "type", "x", "y", "button"])
        XCTAssertEqual(Set(events[2].keys), ["t", "type", "label"])
        XCTAssertEqual(events[2]["type"] as? String, "marker")
        XCTAssertEqual(try JSON.decoder.decode(Timeline.self, from: JSON.encoder.encode(timeline)), timeline)
    }

    func testTimelineSortsEventsAndAveragesCursor() {
        let timeline = Timeline(events: [
            .marker(t: 2, label: "b"),
            .cursor(t: 1, at: NormalizedPoint(x: 0.2, y: 0.4)),
            .cursor(t: 0.5, at: NormalizedPoint(x: 0.6, y: 0.8)),
            .click(t: 1.5, at: NormalizedPoint(x: 1, y: 1), button: .right),
        ])
        XCTAssertEqual(timeline.events.map(\.t), [0.5, 1, 1.5, 2])
        XCTAssertEqual(timeline.meanCursor!.x, 0.4, accuracy: 1e-9)
        XCTAssertEqual(timeline.meanCursor!.y, 0.6, accuracy: 1e-9)
        XCTAssertNil(Timeline(events: [.marker(t: 1, label: "x")]).meanCursor)
    }

    func testMarkerLinesSkipGarbage() {
        let lines = MarkerLine.parse(jsonl: "{\"hostTime\":10.5,\"label\":\"a\"}\nnot json\n{\"hostTime\":12,\"label\":\"b\"}\n")
        XCTAssertEqual(lines, [MarkerLine(hostTime: 10.5, label: "a"), MarkerLine(hostTime: 12, label: "b")])
    }

    func testTakeClockOffsets() {
        let clock = TakeClock(t0: 1000)
        XCTAssertEqual(clock.offset(firstSampleHost: 1000.1234), 0.123)
        XCTAssertEqual(clock.offset(firstSampleHost: 999.9), 0)
        XCTAssertEqual(clock.time(ofHost: 1003.9), 3.9, accuracy: 1e-9)
    }
}
