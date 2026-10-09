@preconcurrency import AVFoundation
import CoreMedia
import Foundation

final class AudioTrackWriter: @unchecked Sendable {
    let url: URL
    let queue = DispatchQueue(label: "agentcam.writer.mic")
    private var writer: AVAssetWriter?
    private var input: AVAssetWriterInput?
    private(set) var firstHostTime: Double?
    private(set) var failure: Error?

    init(url: URL) {
        self.url = url
    }

    func append(_ sample: CMSampleBuffer, hostTime: CMTime) {
        guard failure == nil else { return }
        if writer == nil {
            do {
                try start(hint: CMSampleBufferGetFormatDescription(sample), at: hostTime)
            } catch {
                failure = error
                return
            }
            firstHostTime = hostTime.seconds
        }
        guard let input, input.isReadyForMoreMediaData, let retimed = sample.retimed(to: hostTime) else { return }
        if !input.append(retimed) {
            failure = writer?.error ?? CommandError(.recorderFailed, "could not append to mic.m4a")
        }
    }

    private func start(hint: CMFormatDescription?, at time: CMTime) throws {
        try? FileManager.default.removeItem(at: url)
        let writer = try AVAssetWriter(outputURL: url, fileType: .m4a)
        var sampleRate = 48_000.0
        var channels = 1
        if let hint, let asbd = CMAudioFormatDescriptionGetStreamBasicDescription(hint)?.pointee {
            sampleRate = asbd.mSampleRate
            channels = Int(min(asbd.mChannelsPerFrame, 2))
        }
        let input = AVAssetWriterInput(mediaType: .audio, outputSettings: [
            AVFormatIDKey: kAudioFormatMPEG4AAC,
            AVSampleRateKey: sampleRate,
            AVNumberOfChannelsKey: channels,
            AVEncoderBitRateKey: 128_000,
        ], sourceFormatHint: hint)
        input.expectsMediaDataInRealTime = true
        guard writer.canAdd(input) else { throw CommandError(.recorderFailed, "writer rejected audio input") }
        writer.add(input)
        guard writer.startWriting() else {
            throw writer.error ?? CommandError(.recorderFailed, "could not start mic.m4a")
        }
        writer.startSession(atSourceTime: time)
        self.writer = writer
        self.input = input
    }

    func finish(endHostTime: CMTime) async -> Bool {
        let (writer, input): (AVAssetWriter?, AVAssetWriterInput?) = queue.sync { (self.writer, self.input) }
        guard let writer, let input else { return false }
        await withCheckedContinuation { (done: CheckedContinuation<Void, Never>) in
            queue.async {
                input.markAsFinished()
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

/// One AVCaptureSession for the webcam and mic, so the preview layer can share it.
final class CameraRecorder: NSObject, AVCaptureVideoDataOutputSampleBufferDelegate, AVCaptureAudioDataOutputSampleBufferDelegate {
    let session = AVCaptureSession()
    let video: VideoTrackWriter?
    let audio: AudioTrackWriter?
    private let videoOutput = AVCaptureVideoDataOutput()
    private let audioOutput = AVCaptureAudioDataOutput()
    private var t0: Double?
    private var onWarm: (() -> Void)?
    /// Recent luma readings, touched only on the video queue.
    private var recentLuma: [Double] = []
    private let gate = NSLock()

    init(camera: URL?, mic: URL?) {
        video = camera.map { VideoTrackWriter(url: $0, label: "camera") }
        audio = mic.map { AudioTrackWriter(url: $0) }
    }

    static func ensurePermission(for media: AVMediaType) async throws {
        let name = media == .video ? "Camera" : "Microphone"
        let flag = media == .video ? "--no-cam" : "--no-mic"
        switch AVCaptureDevice.authorizationStatus(for: media) {
        case .authorized:
            return
        case .notDetermined:
            if await AVCaptureDevice.requestAccess(for: media) { return }
        default:
            break
        }
        throw CommandError(
            .permissionDenied,
            "\(name) permission is not granted. Allow it for your terminal app in System Settings > Privacy & Security > \(name), or pass \(flag)."
        )
    }

    func configure() throws {
        session.beginConfiguration()
        defer { session.commitConfiguration() }

        if let video {
            guard let device = AVCaptureDevice.default(for: .video) else {
                throw CommandError(.sourceNotFound, "no camera found; pass --no-cam")
            }
            let input = try AVCaptureDeviceInput(device: device)
            guard session.canAddInput(input) else { throw CommandError(.recorderFailed, "cannot use camera \(device.localizedName)") }
            session.addInput(input)
            if session.canSetSessionPreset(.hd1920x1080) {
                session.sessionPreset = .hd1920x1080
            } else {
                session.sessionPreset = .high
            }
            videoOutput.videoSettings = [kCVPixelBufferPixelFormatTypeKey as String: kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange]
            videoOutput.alwaysDiscardsLateVideoFrames = true
            videoOutput.setSampleBufferDelegate(self, queue: video.queue)
            guard session.canAddOutput(videoOutput) else { throw CommandError(.recorderFailed, "cannot add camera output") }
            session.addOutput(videoOutput)
        }

        if let audio {
            guard let device = AVCaptureDevice.default(for: .audio) else {
                throw CommandError(.sourceNotFound, "no microphone found; pass --no-mic")
            }
            let input = try AVCaptureDeviceInput(device: device)
            guard session.canAddInput(input) else { throw CommandError(.recorderFailed, "cannot use microphone \(device.localizedName)") }
            session.addInput(input)
            audioOutput.setSampleBufferDelegate(self, queue: audio.queue)
            guard session.canAddOutput(audioOutput) else { throw CommandError(.recorderFailed, "cannot add microphone output") }
            session.addOutput(audioOutput)
        }
    }

    func startRunning() {
        session.startRunning()
    }

    /// Webcams deliver black frames while auto-exposure settles, so the take must not begin until
    /// the picture is steady. The timeout keeps a dark room from blocking the take.
    func waitUntilWarm(timeout: Double = 3) async {
        guard video != nil else { return }
        await withCheckedContinuation { (done: CheckedContinuation<Void, Never>) in
            let once = OnceFlag()
            let resume = { if once.claim() { done.resume() } }
            gate.withLock { onWarm = resume }
            DispatchQueue.global().asyncAfter(deadline: .now() + timeout) {
                if once.claim() {
                    Output.log("camera still dark after \(timeout)s; starting anyway")
                    done.resume()
                }
            }
        }
        gate.withLock { onWarm = nil }
    }

    /// Exposure ramps up over several frames; settled means a lit picture that has stopped changing.
    private func exposureSettled(_ luma: Double) -> Bool {
        recentLuma = Array((recentLuma + [luma]).suffix(6))
        guard recentLuma.count == 6, let low = recentLuma.min(), let high = recentLuma.max() else { return false }
        return low > 20 && high - low < 2
    }

    /// Samples stamped before t0 are dropped so every track's offset is a true non-negative delay.
    func beginRecording(t0: Double) {
        gate.withLock { self.t0 = t0 }
    }

    func stop() {
        gate.withLock { t0 = nil }
        session.stopRunning()
    }

    func captureOutput(_ output: AVCaptureOutput, didOutput sample: CMSampleBuffer, from connection: AVCaptureConnection) {
        let (t0, onWarm) = gate.withLock { (self.t0, self.onWarm) }
        if let onWarm, output === videoOutput, exposureSettled(sample.meanLuma) { onWarm() }
        guard let t0 else { return }
        let pts = CMSampleBufferGetPresentationTimeStamp(sample)
        let host = CMSyncConvertTime(pts, from: session.synchronizationClock ?? CMClockGetHostTimeClock(), to: CMClockGetHostTimeClock())
        guard host.seconds >= t0 else { return }
        if output === videoOutput {
            video?.append(sample, hostTime: host)
        } else if output === audioOutput {
            audio?.append(sample, hostTime: host)
        }
    }
}

private final class OnceFlag: @unchecked Sendable {
    private var claimed = false
    private let lock = NSLock()
    func claim() -> Bool {
        lock.withLock {
            defer { claimed = true }
            return !claimed
        }
    }
}

private extension CMSampleBuffer {
    /// Average of a sparse grid over the luma plane; video-range black reads 16.
    var meanLuma: Double {
        guard let pixels = CMSampleBufferGetImageBuffer(self) else { return 0 }
        CVPixelBufferLockBaseAddress(pixels, .readOnly)
        defer { CVPixelBufferUnlockBaseAddress(pixels, .readOnly) }
        guard let base = CVPixelBufferGetBaseAddressOfPlane(pixels, 0) else { return 0 }
        let width = CVPixelBufferGetWidthOfPlane(pixels, 0)
        let height = CVPixelBufferGetHeightOfPlane(pixels, 0)
        let stride = CVPixelBufferGetBytesPerRowOfPlane(pixels, 0)
        let bytes = base.assumingMemoryBound(to: UInt8.self)
        var total = 0, count = 0
        for y in Swift.stride(from: 0, to: height, by: max(1, height / 32)) {
            for x in Swift.stride(from: 0, to: width, by: max(1, width / 32)) {
                total += Int(bytes[y * stride + x])
                count += 1
            }
        }
        return count == 0 ? 0 : Double(total) / Double(count)
    }
}
