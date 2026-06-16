import Foundation

public struct TunnelStatusPayload: Codable {
    public let managerInstalled: Bool
    public let managerEnabled: Bool
    public let providerRunning: Bool
    public let routeReady: Bool
    public let tunnelIp: String
    public let sunshineReachable: Bool
    public let state: String
    public let lastError: String?

    public init(
        managerInstalled: Bool,
        managerEnabled: Bool,
        providerRunning: Bool,
        routeReady: Bool,
        tunnelIp: String,
        sunshineReachable: Bool,
        state: String,
        lastError: String?
    ) {
        self.managerInstalled = managerInstalled
        self.managerEnabled = managerEnabled
        self.providerRunning = providerRunning
        self.routeReady = routeReady
        self.tunnelIp = tunnelIp
        self.sunshineReachable = sunshineReachable
        self.state = state
        self.lastError = lastError
    }

    public static func error(_ message: String, state: String = "error") -> TunnelStatusPayload {
        TunnelStatusPayload(
            managerInstalled: false,
            managerEnabled: false,
            providerRunning: false,
            routeReady: false,
            tunnelIp: "",
            sunshineReachable: false,
            state: state,
            lastError: message
        )
    }
}

public struct TunnelBridgeRequest: Codable {
    public let command: String
    public let session: TunnelSessionPayload
}
