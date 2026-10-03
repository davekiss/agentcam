import AppKit
import ArgumentParser
import Foundation
import RecKit

struct Rec: ParsableCommand {
    static let configuration = CommandConfiguration(
        commandName: "rec",
        abstract: "Agent-first screen recorder. Every command prints one JSON object to stdout.",
        subcommands: [Sources.self, Start.self, Stop.self, Status.self, Mark.self, Record.self, Export.self]
    )
}

struct CaptureOptions: ParsableArguments {
    @Option(help: "Display id to capture (see `rec sources`).") var display: UInt32?
    @Option(help: "Window id to capture (see `rec sources`).") var window: UInt32?
    @Option(help: "Capture this app's frontmost window.") var app: String?
    @Flag(help: "Do not record the webcam.") var noCam = false
    @Flag(help: "Do not record the microphone.") var noMic = false
    @Flag(help: "Do not show the floating webcam preview.") var noPreview = false
    @Option(help: "Parent folder for the take (default ~/Movies/rec).") var out: String?

    func validate() throws {
        if [display != nil, window != nil, app != nil].filter({ $0 }).count > 1 {
            throw ValidationError("pass at most one of --display, --window, --app")
        }
    }

    var selector: SourceSelector {
        if let display { return .display(display) }
        if let window { return .window(window) }
        if let app { return .app(app) }
        return .mainDisplay
    }

    func recordOptions(takeDir: URL, duration: Double?) -> RecordOptions {
        RecordOptions(selector: selector, camera: !noCam, mic: !noMic, preview: !noPreview, takeDir: takeDir, duration: duration)
    }

    /// The flags as `rec record` expects them, for the detached child.
    var forwarded: [String] {
        var args: [String] = []
        if let display { args += ["--display", String(display)] }
        if let window { args += ["--window", String(window)] }
        if let app { args += ["--app", app] }
        if noCam { args.append("--no-cam") }
        if noMic { args.append("--no-mic") }
        if noPreview { args.append("--no-preview") }
        return args
    }
}

/// Commands that need async work run it on a Task while the main queue keeps serving the main actor.
protocol AsyncCommand {
    func execute() async throws
}

struct Sources: ParsableCommand, AsyncCommand {
    static let configuration = CommandConfiguration(abstract: "List displays and shareable windows.")

    func execute() async throws {
        Output.emit(try await SourceCatalog.list())
    }
}

struct Start: ParsableCommand, AsyncCommand {
    static let configuration = CommandConfiguration(abstract: "Start a background recorder; returns once it is recording.")
    @OptionGroup var capture: CaptureOptions

    func execute() async throws {
        let takeDir = Control.newTakeDir(root: capture.out.map { URL(fileURLWithPath: $0) })
        try await Control.preflight(capture.recordOptions(takeDir: takeDir, duration: nil))
        let result = try Control.start(executable: executablePath(), recordArguments: capture.forwarded, takeDir: takeDir)
        Output.emit(result)
    }
}

struct Stop: ParsableCommand {
    static let configuration = CommandConfiguration(abstract: "Stop the active recorder and print the finished take.json.")

    func run() throws {
        if let take = try Control.stop() {
            Output.emit(take)
        } else {
            Output.emit(StatusResult(recording: false))
        }
    }
}

struct Status: ParsableCommand {
    static let configuration = CommandConfiguration(abstract: "Report whether a take is recording.")

    func run() throws {
        Output.emit(Control.status())
    }
}

struct Mark: ParsableCommand {
    static let configuration = CommandConfiguration(abstract: "Add a marker at the current moment of the active take.")
    @Argument var label: String

    func run() throws {
        Output.emit(try Control.mark(label))
    }
}

struct Record: ParsableCommand {
    static let configuration = CommandConfiguration(abstract: "Record in the foreground until --duration ends or SIGINT.")
    @Option(help: "Seconds to record.") var duration: Double?
    @OptionGroup var capture: CaptureOptions
    @Option(help: .hidden) var takeDir: String?

    func run() throws {
        let dir = takeDir.map { URL(fileURLWithPath: $0) } ?? Control.newTakeDir(root: capture.out.map { URL(fileURLWithPath: $0) })
        let options = capture.recordOptions(takeDir: dir, duration: duration)
        MainActor.assumeIsolated {
            let recorder = Recorder(options: options)
            let app = NSApplication.shared
            app.setActivationPolicy(.accessory)
            Task { await recorder.run() }
            app.run()
        }
    }
}

extension Aspect: ExpressibleByArgument {}

struct Export: ParsableCommand, AsyncCommand {
    static let configuration = CommandConfiguration(abstract: "Compose a take into 16:9 and/or 9:16 MP4s.")
    @Argument(help: "Path to the take folder.") var take: String
    @Option(help: "16:9 or 9:16; repeat for both (default both).") var layout: [Aspect] = []
    @Flag(help: "Do not draw the attention border.") var noBorder = false

    func execute() async throws {
        let aspects = layout.isEmpty ? Aspect.allCases : Aspect.allCases.filter(layout.contains)
        Output.emit(try await Exporter.export(take: URL(fileURLWithPath: take), aspects: aspects, border: !noBorder))
    }
}

func executablePath() -> String {
    Bundle.main.executableURL?.resolvingSymlinksInPath().path ?? URL(fileURLWithPath: CommandLine.arguments[0]).standardizedFileURL.path
}

let command: ParsableCommand
do {
    command = try Rec.parseAsRoot()
} catch {
    if Rec.exitCode(for: error) == .success { Rec.exit(withError: error) }
    Output.fail(CommandError(.invalidArgument, Rec.message(for: error)))
}

if let asyncCommand = command as? AsyncCommand {
    Task {
        do {
            try await asyncCommand.execute()
            exit(0)
        } catch {
            Output.fail(error)
        }
    }
    dispatchMain()
} else {
    var command = command
    do {
        try command.run()
    } catch {
        Output.fail(error)
    }
}
