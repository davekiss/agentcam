import Foundation

public struct CommandError: Error, Codable, Equatable, Sendable {
    public enum Code: String, Codable, Sendable {
        case permissionDenied = "permission_denied"
        case sourceNotFound = "source_not_found"
        case alreadyRecording = "already_recording"
        case notRecording = "not_recording"
        case recorderFailed = "recorder_failed"
        case invalidTake = "invalid_take"
        case exportFailed = "export_failed"
        case invalidArgument = "invalid_argument"
        case timeout
        case `internal`
    }

    public var code: Code
    public var message: String

    public init(_ code: Code, _ message: String) {
        self.code = code
        self.message = message
    }
}

extension CommandError: LocalizedError {
    public var errorDescription: String? { "\(code.rawValue): \(message)" }
}

public enum Output {
    public static func emit<T: Encodable>(_ value: T) {
        do {
            let data = try JSON.encoder.encode(value)
            FileHandle.standardOutput.write(data)
            FileHandle.standardOutput.write(Data("\n".utf8))
        } catch {
            fail(CommandError(.internal, "could not encode output: \(error)"))
        }
    }

    public static func fail(_ error: Error) -> Never {
        let commandError = (error as? CommandError) ?? CommandError(.internal, "\(error)")
        struct Envelope: Encodable { let error: CommandError }
        if let data = try? JSON.encoder.encode(Envelope(error: commandError)) {
            FileHandle.standardOutput.write(data)
            FileHandle.standardOutput.write(Data("\n".utf8))
        }
        exit(1)
    }

    public static func log(_ message: String) {
        FileHandle.standardError.write(Data("[rec] \(message)\n".utf8))
    }
}
