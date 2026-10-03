import Foundation

public struct StartResult: Encodable {
    public var take: String
    public var pid: Int32
}

public struct StatusResult: Encodable {
    public var recording: Bool
    public var take: String?
    public var elapsed: Double?

    public init(recording: Bool, take: String? = nil, elapsed: Double? = nil) {
        self.recording = recording
        self.take = take
        self.elapsed = elapsed
    }
}

public struct MarkResult: Encodable {
    public var take: String
    public var t: Double
    public var label: String
}

public enum Control {
    public static var defaultRoot: URL {
        FileManager.default.homeDirectoryForCurrentUser.appendingPathComponent("Movies/rec")
    }

    /// Two takes started in the same second get distinct folders.
    public static func newTakeDir(root: URL?) -> URL {
        let base = (root ?? defaultRoot).appendingPathComponent(TakeID.make())
        var candidate = base
        var n = 2
        while FileManager.default.fileExists(atPath: candidate.path) {
            candidate = base.deletingLastPathComponent().appendingPathComponent("\(base.lastPathComponent)-\(n)")
            n += 1
        }
        return candidate
    }

    /// Checks permissions before spawning so the common failures come back as a precise error.
    /// It must not touch SCShareableContent: while this process holds a ScreenCaptureKit
    /// connection, the child's SCStream.startCapture never returns (observed on macOS 15.3).
    public static func preflight(_ options: RecordOptions) async throws {
        if let active = ActiveStore.current() {
            throw CommandError(.alreadyRecording, "already recording \(active.take) (pid \(active.pid)); run `rec stop` first")
        }
        try SourceCatalog.ensureScreenRecordingPermission()
        if options.camera { try await CameraRecorder.ensurePermission(for: .video) }
        if options.mic { try await CameraRecorder.ensurePermission(for: .audio) }
    }

    public static func start(executable: String, recordArguments: [String], takeDir: URL, timeout: Double = 20) throws -> StartResult {
        try FileManager.default.createDirectory(at: takeDir, withIntermediateDirectories: true)
        let log = takeDir.appendingPathComponent(TakeFile.log)
        let pid = try spawnDetached(executable: executable, arguments: ["record", "--take-dir", takeDir.path] + recordArguments, log: log)

        let manifest = takeDir.appendingPathComponent(TakeFile.manifest)
        let deadline = Date().addingTimeInterval(timeout)
        while Date() < deadline {
            if let take = try? JSON.read(Take.self, from: manifest) {
                switch take.status {
                case .recording: return StartResult(take: takeDir.path, pid: pid)
                case .failed, .finished:
                    reap(pid)
                    throw take.error ?? CommandError(.recorderFailed, "recorder ended early: \(logTail(log))")
                }
            }
            var status: Int32 = 0
            if waitpid(pid, &status, WNOHANG) == pid {
                if let take = try? JSON.read(Take.self, from: manifest), let error = take.error { throw error }
                if let error = lastError(inLog: log) { throw error }
                throw CommandError(.recorderFailed, "recorder exited before recording: \(logTail(log))")
            }
            usleep(100_000)
        }
        kill(pid, SIGKILL)
        reap(pid)
        throw CommandError(.timeout, "recorder did not start within \(Int(timeout))s: \(logTail(log))")
    }

    public static func stop(timeout: Double = 60) throws -> Take? {
        guard let active = ActiveStore.current() else { return nil }
        kill(active.pid, SIGINT)
        let deadline = Date().addingTimeInterval(timeout)
        while ActiveStore.isAlive(active.pid), Date() < deadline {
            usleep(100_000)
        }
        let manifest = active.takeURL.appendingPathComponent(TakeFile.manifest)
        if ActiveStore.isAlive(active.pid) {
            throw CommandError(.timeout, "recorder pid \(active.pid) did not finish within \(Int(timeout))s")
        }
        guard let take = try? JSON.read(Take.self, from: manifest) else {
            throw CommandError(.recorderFailed, "recorder exited without a readable take.json: \(logTail(active.takeURL.appendingPathComponent(TakeFile.log)))")
        }
        switch take.status {
        case .finished: return take
        case .failed: throw take.error ?? CommandError(.recorderFailed, "take failed; see \(active.take)/recorder.log")
        case .recording: throw CommandError(.recorderFailed, "recorder exited while still recording: \(logTail(active.takeURL.appendingPathComponent(TakeFile.log)))")
        }
    }

    public static func status() -> StatusResult {
        guard let active = ActiveStore.current() else { return StatusResult(recording: false) }
        return StatusResult(recording: true, take: active.take, elapsed: active.elapsed())
    }

    public static func mark(_ label: String) throws -> MarkResult {
        guard let active = ActiveStore.current() else {
            throw CommandError(.notRecording, "nothing is recording")
        }
        let line = MarkerLine(t: active.elapsed(), label: label)
        var data = try JSONEncoder().encode(line)
        data.append(0x0A)
        let url = active.takeURL.appendingPathComponent(TakeFile.markers)
        let fd = open(url.path, O_WRONLY | O_CREAT | O_APPEND, 0o644)
        guard fd >= 0 else { throw CommandError(.internal, "cannot open \(url.path)") }
        defer { close(fd) }
        // A single write with O_APPEND keeps concurrent marks from interleaving.
        let written = data.withUnsafeBytes { write(fd, $0.baseAddress, $0.count) }
        guard written == data.count else { throw CommandError(.internal, "short write to \(url.path)") }
        return MarkResult(take: active.take, t: line.t, label: label)
    }

    private static func spawnDetached(executable: String, arguments: [String], log: URL) throws -> pid_t {
        var actions: posix_spawn_file_actions_t?
        posix_spawn_file_actions_init(&actions)
        defer { posix_spawn_file_actions_destroy(&actions) }
        posix_spawn_file_actions_addopen(&actions, 0, "/dev/null", O_RDONLY, 0)
        posix_spawn_file_actions_addopen(&actions, 1, log.path, O_WRONLY | O_CREAT | O_APPEND, 0o644)
        posix_spawn_file_actions_adddup2(&actions, 1, 2)

        var attributes: posix_spawnattr_t?
        posix_spawnattr_init(&attributes)
        defer { posix_spawnattr_destroy(&attributes) }
        // A new session keeps the recorder alive when the calling terminal or agent shell exits.
        posix_spawnattr_setflags(&attributes, Int16(POSIX_SPAWN_SETSID))

        let argv = ([executable] + arguments).map { strdup($0) } + [nil]
        defer { argv.forEach { free($0) } }
        var pid: pid_t = 0
        let rc = posix_spawn(&pid, executable, &actions, &attributes, argv, environ)
        guard rc == 0 else { throw CommandError(.internal, "could not spawn recorder: \(String(cString: strerror(rc)))") }
        return pid
    }

    private static func reap(_ pid: pid_t) {
        var status: Int32 = 0
        for _ in 0..<20 where waitpid(pid, &status, WNOHANG) == 0 {
            usleep(50_000)
        }
    }

    /// The recorder's stdout is in recorder.log, and a failing recorder's last output is its
    /// error envelope.
    static func lastError(inLog url: URL) -> CommandError? {
        struct Envelope: Decodable { let error: CommandError }
        guard let text = try? String(contentsOf: url, encoding: .utf8),
              let start = text.range(of: "{\n  \"error\"", options: .backwards) else { return nil }
        return try? JSONDecoder().decode(Envelope.self, from: Data(text[start.lowerBound...].utf8)).error
    }

    static func logTail(_ url: URL, lines: Int = 8) -> String {
        guard let text = try? String(contentsOf: url, encoding: .utf8) else { return "(no recorder.log)" }
        return text.split(separator: "\n").suffix(lines).joined(separator: " | ")
    }
}
