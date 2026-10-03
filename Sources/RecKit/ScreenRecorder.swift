@preconcurrency import AVFoundation
@preconcurrency import CoreMedia
import Foundation
@preconcurrency import ScreenCaptureKit

/// Writes one H.264 track from timestamped sample buffers. Shared by the screen and camera tracks.
/// All calls happen on `queue`.
final class VideoTrackWriter: @unchecked Sendable {
    let url: URL
    let queue: DispatchQueue
    private var writer: AVAssetWriter?
    private var input: AVAssetWriterInput?
    private(set) var firstHostTime: Double?
    private(set) var lastSample: CMSampleBuffer?
    private(set) var size: CGSize?
    private(set) var failure: Error?

    init(url: URL, label: String) {
        self.url = url
        self.queue = DispatchQueue(label: "rec.writer.\(label)")
    }

    /// `hostTime` is the sample's presentation time on the host clock.
    func append(_ sample: CMSampleBuffer, hostTime: CMTime) {
        guard failure == nil else { return }
        if writer == nil {
            guard let image = CMSampleBufferGetImageBuffer(sample) else { return }
            let size = CGSize(width: CVPixelBufferGetWidth(image), height: CVPixelBufferGetHeight(image))
            do {
                try start(size: size, at: hostTime)
            } catch {
                failure = error
                return
            }
            firstHostTime = hostTime.seconds
            self.size = size
        }
        guard let input, input.isReadyForMoreMediaData else { return }
        guard let retimed = sample.retimed(to: hostTime) else { return }
        if !input.append(retimed) {
            failure = writer?.error ?? CommandError(.recorderFailed, "could not append to \(url.lastPathComponent)")
            return
        }
        lastSample = retimed
    }

    private func start(size: CGSize, at time: CMTime) throws {
        try? FileManager.default.removeItem(at: url)
        let writer = try AVAssetWriter(outputURL: url, fileType: .mov)
        let pixels = Double(size.width * size.height)
        let input = AVAssetWriterInput(mediaType: .video, outputSettings: [
            AVVideoCodecKey: AVVideoCodecType.h264,
            AVVideoWidthKey: Int(size.width),
            AVVideoHeightKey: Int(size.height),
            AVVideoCompressionPropertiesKey: [
                AVVideoAverageBitRateKey: Int(min(max(pixels * 4, 4_000_000), 40_000_000)),
                AVVideoProfileLevelKey: AVVideoProfileLevelH264HighAutoLevel,
                AVVideoAllowFrameReorderingKey: false,
            ],
        ])
        input.expectsMediaDataInRealTime = true
        guard writer.canAdd(input) else { throw CommandError(.recorderFailed, "writer rejected video input") }
        writer.add(input)
        guard writer.startWriting() else {
            throw writer.error ?? CommandError(.recorderFailed, "could not start \(url.lastPathComponent)")
        }
        writer.startSession(atSourceTime: time)
        self.writer = writer
        self.input = input
    }

    /// Repeats the last frame at `endHostTime` so a static screen still fills the whole take,
    /// then finalizes the file. Returns false when nothing was ever written.
    func finish(endHostTime: CMTime) async -> Bool {
        let (writer, input): (AVAssetWriter?, AVAssetWriterInput?) = queue.sync { (self.writer, self.input) }
        guard let writer, let input else { return false }
        await withCheckedContinuation { (done: CheckedContinuation<Void, Never>) in
            queue.async {
                let last = self.lastSample
                if let last, endHostTime > CMSampleBufferGetPresentationTimeStamp(last),
                   input.isReadyForMoreMediaData, let tail = last.retimed(to: endHostTime) {
                    input.append(tail)
                }
                input.markAsFinished()
                writer.endSession(atSourceTime: max(endHostTime, last.map(CMSampleBufferGetPresentationTimeStamp) ?? endHostTime))
                writer.finishWriting { done.resume() }
            }
        }
        if writer.status == .failed {
            queue.sync { self.failure = writer.error }
            return false
        }
        return true
    }
}

extension CMSampleBuffer {
    func retimed(to pts: CMTime) -> CMSampleBuffer? {
        var timing = CMSampleTimingInfo(duration: .invalid, presentationTimeStamp: pts, decodeTimeStamp: .invalid)
        var out: CMSampleBuffer?
        CMSampleBufferCreateCopyWithNewTiming(allocator: nil, sampleBuffer: self, sampleTimingEntryCount: 1, sampleTimingArray: &timing, sampleBufferOut: &out)
        return out
    }
}

final class ScreenRecorder: NSObject, SCStreamOutput, SCStreamDelegate {
    let writer: VideoTrackWriter
    private var stream: SCStream?
    var onFirstFrame: (() -> Void)?
    var onError: ((Error) -> Void)?
    private var sawFirstFrame = false

    init(url: URL) {
        writer = VideoTrackWriter(url: url, label: "screen")
    }

    func start(filter: SCContentFilter, pixelSize: CGSize) async throws {
        let config = SCStreamConfiguration()
        config.width = Int(pixelSize.width)
        config.height = Int(pixelSize.height)
        config.minimumFrameInterval = CMTime(value: 1, timescale: 60)
        config.pixelFormat = kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange
        config.showsCursor = true
        config.queueDepth = 6
        config.capturesAudio = false

        let stream = SCStream(filter: filter, configuration: config, delegate: self)
        try stream.addStreamOutput(self, type: .screen, sampleHandlerQueue: writer.queue)
        self.stream = stream
        do {
            try await stream.startCapture()
        } catch {
            let ns = error as NSError
            if ns.domain == SCStreamErrorDomain, ns.code == SCStreamError.userDeclined.rawValue {
                throw SourceCatalog.screenPermissionError
            }
            throw CommandError(.recorderFailed, "screen capture failed to start: \(error.localizedDescription)")
        }
    }

    func stop() async {
        try? await stream?.stopCapture()
        stream = nil
    }

    func stream(_ stream: SCStream, didOutputSampleBuffer sample: CMSampleBuffer, of type: SCStreamOutputType) {
        guard type == .screen, sample.isValid, isComplete(sample) else { return }
        writer.append(sample, hostTime: CMSampleBufferGetPresentationTimeStamp(sample))
        if let failure = writer.failure {
            onError?(failure)
        } else if !sawFirstFrame, writer.firstHostTime != nil {
            sawFirstFrame = true
            onFirstFrame?()
        }
    }

    func stream(_ stream: SCStream, didStopWithError error: Error) {
        onError?(CommandError(.recorderFailed, "screen capture stopped: \(error.localizedDescription)"))
    }

    private func isComplete(_ sample: CMSampleBuffer) -> Bool {
        guard let attachments = CMSampleBufferGetSampleAttachmentsArray(sample, createIfNecessary: false) as? [[SCStreamFrameInfo: Any]],
              let raw = attachments.first?[.status] as? Int,
              let status = SCFrameStatus(rawValue: raw) else { return false }
        return status == .complete
    }
}
