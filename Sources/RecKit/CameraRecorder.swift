@preconcurrency import AVFoundation
import CoreMedia
import Foundation

final class AudioTrackWriter: @unchecked Sendable {
    let url: URL
    let queue = DispatchQueue(label: "rec.writer.mic")
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

    /// Samples stamped before t0 are dropped so every track's offset is a true non-negative delay.
    func beginRecording(t0: Double) {
        gate.withLock { self.t0 = t0 }
    }

    func stop() {
        gate.withLock { t0 = nil }
        session.stopRunning()
    }

    func captureOutput(_ output: AVCaptureOutput, didOutput sample: CMSampleBuffer, from connection: AVCaptureConnection) {
        guard let t0 = gate.withLock({ t0 }) else { return }
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
