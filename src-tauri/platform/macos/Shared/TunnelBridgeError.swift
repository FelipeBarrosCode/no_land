import Foundation

public enum TunnelBridgeError: LocalizedError {
    case invalidRequest(String)
    case managerUnavailable(String)
    case managerSaveFailed(String)
    case tunnelStartFailed(String)
    case tunnelStopFailed(String)

    public var errorDescription: String? {
        switch self {
        case .invalidRequest(let message),
             .managerUnavailable(let message),
             .managerSaveFailed(let message),
             .tunnelStartFailed(let message),
             .tunnelStopFailed(let message):
            return message
        }
    }
}
