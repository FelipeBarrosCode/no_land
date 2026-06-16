import Foundation
import Shared

let semaphore = DispatchSemaphore(value: 0)
var exitCode: Int32 = 0

Task {
    do {
        let command = CommandLine.arguments.dropFirst().first ?? "status"
        let request = try decodeRequest()
        let controller = TunnelController()

        let result: TunnelStatusPayload
        switch command {
        case "start":
            result = try await controller.start(session: request.session)
        case "stop":
            result = try await controller.stop(session: request.session)
        case "status":
            result = try await controller.status(session: request.session)
        default:
            throw TunnelBridgeError.invalidRequest("Unsupported bridge command: \(command)")
        }

        try printStatus(result)
    } catch {
        let payload = TunnelStatusPayload.error(error.localizedDescription)
        try? printStatus(payload)
        fputs("\(error.localizedDescription)\n", stderr)
        exitCode = 1
    }
    semaphore.signal()
}

semaphore.wait()
Foundation.exit(exitCode)

private func decodeRequest() throws -> TunnelBridgeRequest {
    let input = FileHandle.standardInput.readDataToEndOfFile()
    guard !input.isEmpty else {
        throw TunnelBridgeError.invalidRequest("Bridge request JSON was empty")
    }
    return try JSONDecoder().decode(TunnelBridgeRequest.self, from: input)
}

private func printStatus(_ status: TunnelStatusPayload) throws {
    let data = try JSONEncoder().encode(status)
    if let output = String(data: data, encoding: String.Encoding.utf8) {
        FileHandle.standardOutput.write(output.data(using: String.Encoding.utf8) ?? Data())
    }
}
