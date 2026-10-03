@preconcurrency import AVFoundation
import Foundation

public struct ExportedFile: Encodable {
    public var layout: String
    public var path: String
    public var width: Int
    public var height: Int
    public var duration: Double
}

public struct ExportResult: Encodable {
    public var take: String
    public var exports: [ExportedFile]
}

public enum Exporter {
    public static func export(take takeDir: URL, aspects: [Aspect], border: Bool) async throws -> ExportResult {
        let manifest = takeDir.appendingPathComponent(TakeFile.manifest)
        guard let take = try? JSON.read(Take.self, from: manifest) else {
            throw CommandError(.invalidTake, "no readable take.json in \(takeDir.path)")
        }
        guard take.status == .finished else {
            throw CommandError(.invalidTake, "take status is \(take.status.rawValue); only finished takes export")
        }
        let timeline = try? JSON.read(Timeline.self, from: takeDir.appendingPathComponent(TakeFile.timeline))

        var exports: [ExportedFile] = []
        for aspect in aspects {
            let layout = Layout.preset(aspect)
            let out = takeDir.appendingPathComponent("export-\(aspect.fileSuffix).mp4")
            let duration = try await export(take: take, in: takeDir, layout: layout, focusX: timeline?.meanCursor?.x, border: border, to: out)
            exports.append(ExportedFile(
                layout: aspect.rawValue,
                path: out.path,
                width: Int(layout.canvas.width),
                height: Int(layout.canvas.height),
                duration: (duration * 1000).rounded() / 1000
            ))
        }
        return ExportResult(take: takeDir.path, exports: exports)
    }

    /// Inserts each track at its take-clock offset, so composition time equals take time.
    private static func insert(_ track: Track, from dir: URL, media: AVMediaType, into composition: AVMutableComposition) async throws -> AVMutableCompositionTrack? {
        let url = dir.appendingPathComponent(track.file)
        guard FileManager.default.fileExists(atPath: url.path) else {
            if track.kind == .screen { throw CommandError(.invalidTake, "missing \(track.file)") }
            Output.log("skipping missing \(track.file)")
            return nil
        }
        let asset = AVURLAsset(url: url)
        guard let source = try await asset.loadTracks(withMediaType: media).first else {
            throw CommandError(.invalidTake, "\(track.file) has no \(media.rawValue) track")
        }
        let range = try await source.load(.timeRange)
        guard let target = composition.addMutableTrack(withMediaType: media, preferredTrackID: kCMPersistentTrackID_Invalid) else {
            throw CommandError(.exportFailed, "could not add a \(media.rawValue) track")
        }
        try target.insertTimeRange(range, of: source, at: CMTime(seconds: track.offset, preferredTimescale: 600))
        return target
    }

    private static func export(take: Take, in dir: URL, layout: Layout, focusX: Double?, border: Bool, to out: URL) async throws -> Double {
        guard let screenTrack = take.track(.screen) else {
            throw CommandError(.invalidTake, "take has no screen track")
        }
        let composition = AVMutableComposition()
        guard let screen = try await insert(screenTrack, from: dir, media: .video, into: composition) else {
            throw CommandError(.invalidTake, "missing screen.mov")
        }
        var camera: AVMutableCompositionTrack?
        if let track = take.track(.camera) {
            camera = try await insert(track, from: dir, media: .video, into: composition)
        }
        if let track = take.track(.mic) {
            _ = try await insert(track, from: dir, media: .audio, into: composition)
        }

        let screenEnd = screen.timeRange.end.seconds
        let duration = min(take.duration ?? screenEnd, composition.duration.seconds)
        let exportRange = CMTimeRange(start: .zero, duration: CMTime(seconds: duration, preferredTimescale: 600))

        let renderer = FrameRenderer(layout: layout, focusX: focusX, drawsBorder: border)
        let videoComposition = AVMutableVideoComposition()
        videoComposition.customVideoCompositorClass = LayoutCompositor.self
        videoComposition.renderSize = layout.canvas
        videoComposition.frameDuration = CMTime(value: 1, timescale: 30)
        videoComposition.instructions = [LayoutInstruction(
            timeRange: CMTimeRange(start: .zero, duration: composition.duration),
            screenTrackID: screen.trackID,
            cameraTrackID: camera?.trackID,
            renderer: renderer
        )]

        guard let session = AVAssetExportSession(asset: composition, presetName: AVAssetExportPresetHighestQuality) else {
            throw CommandError(.exportFailed, "no export session for this composition")
        }
        try? FileManager.default.removeItem(at: out)
        session.outputURL = out
        session.outputFileType = .mp4
        session.videoComposition = videoComposition
        session.timeRange = exportRange
        session.shouldOptimizeForNetworkUse = true

        Output.log("exporting \(layout.aspect.rawValue) to \(out.path)")
        await withCheckedContinuation { (done: CheckedContinuation<Void, Never>) in
            session.exportAsynchronously { done.resume() }
        }
        guard session.status == .completed else {
            throw CommandError(.exportFailed, "\(layout.aspect.rawValue) export failed: \(session.error?.localizedDescription ?? "status \(session.status.rawValue)")")
        }
        return duration
    }
}
