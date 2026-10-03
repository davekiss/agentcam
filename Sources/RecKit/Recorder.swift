import AppKit
@preconcurrency import AVFoundation
import Foundation
@preconcurrency import ScreenCaptureKit

public struct RecordOptions: Sendable {
    public var selector: SourceSelector
    public var camera: Bool
    public var mic: Bool
    public var preview: Bool
    public var takeDir: URL
    public var duration: Double?

    public init(selector: SourceSelector, camera: Bool, mic: Bool, preview: Bool, takeDir: URL, duration: Double?) {
        self.selector = selector
        self.camera = camera
        self.mic = mic
        self.preview = preview
        self.takeDir = takeDir
        self.duration = duration
    }
}

enum RecorderState {
    case idle
    case starting
    case recording(TakeClock)
    case stopping
    case finished
    case failed(CommandError)
}

/// The foreground recorder behind `rec record`. Owns the whole lifecycle of one take and exits
/// the process when the take is finished or failed. Runs on the main thread inside NSApplication.
@MainActor
public final class Recorder {
    private let options: RecordOptions
    private var state: RecorderState = .idle
    private var take: Take?
    private var startedAt = Date()
    private var screen: ScreenRecorder?
    private var camera: CameraRecorder?
    private var preview: PreviewPanel?
    private var input: InputMonitor?
    private var signalSources: [DispatchSourceSignal] = []
    private var firstFrame: CheckedContinuation<Void, Error>?

    public init(options: RecordOptions) {
        self.options = options
    }

    private var pid: Int32 { ProcessInfo.processInfo.processIdentifier }
    private func url(_ name: String) -> URL { options.takeDir.appendingPathComponent(name) }

    public func run() async {
        installSignalHandlers()
        do {
            try await start()
        } catch {
            await fail(error)
            return
        }
        if let duration = options.duration, case let .recording(clock) = state {
            let remaining = max(0, duration - clock.time(ofHost: TakeClock.hostNow()))
            Timer.scheduledTimer(withTimeInterval: remaining, repeats: false) { [weak self] _ in
                MainActor.assumeIsolated { self?.requestStop() }
            }
        }
    }

    private func start() async throws {
        guard case .idle = state else { return }
        state = .starting

        if let active = ActiveStore.current(), active.pid != pid {
            throw CommandError(.alreadyRecording, "already recording \(active.take) (pid \(active.pid)); run `rec stop` first")
        }
        var content = try await SourceCatalog.shareableContent()
        let resolved = try await SourceCatalog.resolve(options.selector, in: content)
        try FileManager.default.createDirectory(at: options.takeDir, withIntermediateDirectories: true)
        take = Take(
            id: options.takeDir.lastPathComponent,
            createdAt: Date(),
            status: .recording,
            source: resolved.source
        )
        if options.camera { try await CameraRecorder.ensurePermission(for: .video) }
        if options.mic { try await CameraRecorder.ensurePermission(for: .audio) }

        if options.camera || options.mic {
            let camera = CameraRecorder(
                camera: options.camera ? url(Track.Kind.camera.file) : nil,
                mic: options.mic ? url(Track.Kind.mic.file) : nil
            )
            try camera.configure()
            await Task.detached { camera.startRunning() }.value
            self.camera = camera
        }

        var excluded: [SCWindow] = []
        if options.camera, options.preview, let camera {
            let panel = PreviewPanel(session: camera.session)
            panel.show()
            preview = panel
            excluded = try await findWindow(panel.windowID, refreshing: &content)
        }

        let filter: SCContentFilter
        let pixelSize: CGSize
        let scale = CGFloat(resolved.source.scale)
        switch resolved {
        case let .display(display, _):
            filter = SCContentFilter(display: display, excludingWindows: excluded)
            pixelSize = CGSize(width: CGFloat(display.width) * scale, height: CGFloat(display.height) * scale)
        case let .window(window, _):
            filter = SCContentFilter(desktopIndependentWindow: window)
            pixelSize = CGSize(width: window.frame.width * scale, height: window.frame.height * scale)
        }

        let clock = TakeClock(t0: TakeClock.hostNow())
        startedAt = Date()
        camera?.beginRecording(t0: clock.t0)
        let input = InputMonitor(frame: resolved.source.frame.cgRect) { clock.time(ofHost: TakeClock.hostNow()) }
        self.input = input

        let screen = ScreenRecorder(url: url(Track.Kind.screen.file))
        self.screen = screen
        screen.onFirstFrame = { [weak self] in
            Task { @MainActor in self?.firstFrame?.resume(); self?.firstFrame = nil }
        }
        screen.onError = { [weak self] error in
            Task { @MainActor in self?.captureFailed(error) }
        }
        try await screen.start(filter: filter, pixelSize: pixelSize)
        input.start()

        try await withCheckedThrowingContinuation { (c: CheckedContinuation<Void, Error>) in
            firstFrame = c
            Timer.scheduledTimer(withTimeInterval: 6, repeats: false) { [weak self] _ in
                MainActor.assumeIsolated {
                    self?.firstFrame?.resume(throwing: CommandError(.timeout, "no screen frames arrived within 6s"))
                    self?.firstFrame = nil
                }
            }
        }

        guard case .starting = state else { return }
        state = .recording(clock)
        try JSON.write(take, to: url(TakeFile.manifest))
        try ActiveStore.write(ActiveRecord(pid: pid, take: options.takeDir.path, startedAt: startedAt))
        preview?.playIntro()
        Output.log("recording \(resolved.source.kind.rawValue) \(resolved.source.id) to \(options.takeDir.path)")
    }

    /// The panel takes a moment to appear in ScreenCaptureKit's window list after ordering front.
    private func findWindow(_ id: CGWindowID, refreshing content: inout SCShareableContent) async throws -> [SCWindow] {
        for _ in 0..<10 {
            content = try await SourceCatalog.shareableContent()
            if let w = content.windows.first(where: { $0.windowID == id }) { return [w] }
            try await Task.sleep(nanoseconds: 100_000_000)
        }
        Output.log("preview window not found in shareable content; it may appear in the capture")
        return []
    }

    private func installSignalHandlers() {
        for sig in [SIGINT, SIGTERM] {
            signal(sig, SIG_IGN)
            let source = DispatchSource.makeSignalSource(signal: sig, queue: .main)
            source.setEventHandler { [weak self] in
                MainActor.assumeIsolated { self?.requestStop() }
            }
            source.resume()
            signalSources.append(source)
        }
    }

    private func requestStop() {
        switch state {
        case let .recording(clock):
            state = .stopping
            Task { await self.stop(clock: clock) }
        case .starting, .idle:
            Task { await self.fail(CommandError(.recorderFailed, "interrupted before recording started")) }
        case .stopping, .finished, .failed:
            break
        }
    }

    private func captureFailed(_ error: Error) {
        switch state {
        case .starting:
            firstFrame?.resume(throwing: error)
            firstFrame = nil
        case .recording:
            Task { await self.fail(error) }
        default:
            break
        }
    }

    private func stop(clock: TakeClock) async {
        let result = await finalize(clock: clock)
        guard var take = result else {
            await fail(CommandError(.recorderFailed, "screen.mov could not be finalized"))
            return
        }
        take.status = .finished
        do {
            try JSON.write(take, to: url(TakeFile.manifest))
        } catch {
            await fail(error)
            return
        }
        state = .finished
        ActiveStore.clear(ownedBy: pid)
        Output.log("finished \(options.takeDir.path)")
        Output.emit(take)
        exit(0)
    }

    /// Stops capture, finalizes every file, and writes timeline.json. Returns the take with its
    /// tracks filled in, or nil if the screen track is unusable.
    private func finalize(clock: TakeClock) async -> Take? {
        input?.stop()
        preview?.close()
        await screen?.stop()
        camera?.stop()
        let end = CMClockGetTime(CMClockGetHostTimeClock())

        var tracks: [Track] = []
        if let writer = screen?.writer, await writer.finish(endHostTime: end), let first = writer.firstHostTime {
            tracks.append(Track(kind: .screen, offset: clock.offset(firstSampleHost: first), width: writer.size.map { Int($0.width) }, height: writer.size.map { Int($0.height) }))
        }
        if let writer = camera?.video, await writer.finish(endHostTime: end), let first = writer.firstHostTime {
            tracks.append(Track(kind: .camera, offset: clock.offset(firstSampleHost: first), width: writer.size.map { Int($0.width) }, height: writer.size.map { Int($0.height) }))
        }
        if let writer = camera?.audio, await writer.finish(endHostTime: end), let first = writer.firstHostTime {
            tracks.append(Track(kind: .mic, offset: clock.offset(firstSampleHost: first)))
        }

        let markers = (try? String(contentsOf: url(TakeFile.markers), encoding: .utf8)).map(MarkerLine.parse) ?? []
        let timeline = Timeline(events: (input?.events ?? []) + markers.map { .marker(t: $0.t, label: $0.label) })
        do {
            try JSON.write(timeline, to: url(TakeFile.timeline))
        } catch {
            Output.log("could not write timeline.json: \(error)")
        }

        guard var take, tracks.contains(where: { $0.kind == .screen }) else { return nil }
        take.tracks = tracks
        take.duration = ((clock.time(ofHost: end.seconds)) * 1000).rounded() / 1000
        return take
    }

    private func fail(_ error: Error) async {
        if case .failed = state { return }
        let previous = state
        let commandError = (error as? CommandError) ?? CommandError(.recorderFailed, "\(error)")
        state = .failed(commandError)
        firstFrame?.resume(throwing: commandError)
        firstFrame = nil

        var take = self.take
        if case let .recording(clock) = previous, let finalized = await finalize(clock: clock) {
            take = finalized
        } else {
            input?.stop()
            preview?.close()
            await screen?.stop()
            camera?.stop()
        }
        if var take {
            take.status = .failed
            take.error = commandError
            try? JSON.write(take, to: url(TakeFile.manifest))
        }
        ActiveStore.clear(ownedBy: pid)
        Output.log("failed: \(commandError.message)")
        Output.fail(commandError)
    }
}
