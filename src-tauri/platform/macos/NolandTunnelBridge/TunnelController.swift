import Foundation
import NetworkExtension
import Shared

final class TunnelController {
    func status(session: TunnelSessionPayload) async throws -> TunnelStatusPayload {
        let manager = try await loadManager(matching: session)
        guard let manager else {
            return TunnelStatusPayload(
                managerInstalled: false,
                managerEnabled: false,
                providerRunning: false,
                routeReady: false,
                tunnelIp: session.clientTunnelIp,
                sunshineReachable: false,
                state: "not_configured",
                lastError: nil
            )
        }

        let state = connectionState(manager.connection.status)
        return TunnelStatusPayload(
            managerInstalled: true,
            managerEnabled: manager.isEnabled,
            providerRunning: isRunning(manager.connection.status),
            routeReady: isRunning(manager.connection.status),
            tunnelIp: session.clientTunnelIp,
            sunshineReachable: false,
            state: state,
            lastError: nil
        )
    }

    func start(session: TunnelSessionPayload) async throws -> TunnelStatusPayload {
        let manager = try await loadOrCreateManager(for: session)
        try await save(manager: manager)
        try await load(manager: manager)

        do {
            let options: [String: NSObject]? = nil
            try manager.connection.startVPNTunnel(options: options)
        } catch {
            throw TunnelBridgeError.tunnelStartFailed("Failed to start packet tunnel: \(error.localizedDescription)")
        }

        return TunnelStatusPayload(
            managerInstalled: true,
            managerEnabled: manager.isEnabled,
            providerRunning: true,
            routeReady: false,
            tunnelIp: session.clientTunnelIp,
            sunshineReachable: false,
            state: connectionState(manager.connection.status),
            lastError: nil
        )
    }

    func stop(session: TunnelSessionPayload) async throws -> TunnelStatusPayload {
        guard let manager = try await loadManager(matching: session) else {
            return TunnelStatusPayload(
                managerInstalled: false,
                managerEnabled: false,
                providerRunning: false,
                routeReady: false,
                tunnelIp: session.clientTunnelIp,
                sunshineReachable: false,
                state: "stopped",
                lastError: nil
            )
        }

        manager.connection.stopVPNTunnel()
        return TunnelStatusPayload(
            managerInstalled: true,
            managerEnabled: manager.isEnabled,
            providerRunning: false,
            routeReady: false,
            tunnelIp: session.clientTunnelIp,
            sunshineReachable: false,
            state: "stopped",
            lastError: nil
        )
    }

    private func loadOrCreateManager(for session: TunnelSessionPayload) async throws -> NETunnelProviderManager {
        if let existing = try await loadManager(matching: session) {
            configure(manager: existing, with: session)
            return existing
        }

        let manager = NETunnelProviderManager()
        configure(manager: manager, with: session)
        return manager
    }

    private func configure(manager: NETunnelProviderManager, with session: TunnelSessionPayload) {
        let configuration = NETunnelProviderProtocol()
        configuration.providerBundleIdentifier = SharedConstants.providerBundleIdentifier
        configuration.serverAddress = session.endpointAddress
        configuration.providerConfiguration = session.providerConfiguration
        manager.protocolConfiguration = configuration
        manager.localizedDescription = SharedConstants.managerDescription
        manager.isEnabled = true
        manager.isOnDemandEnabled = false
    }

    private func loadManager(matching session: TunnelSessionPayload) async throws -> NETunnelProviderManager? {
        let managers = try await loadAllManagers()
        return managers.first(where: { manager in
            guard let protocolConfiguration = manager.protocolConfiguration as? NETunnelProviderProtocol else {
                return false
            }
            let matchesBundle = protocolConfiguration.providerBundleIdentifier == SharedConstants.providerBundleIdentifier
            let matchesDescription = manager.localizedDescription == SharedConstants.managerDescription
            let matchesTunnelId = (protocolConfiguration.providerConfiguration?[SharedConstants.Keys.tunnelId] as? String) == session.tunnelId
            return (matchesBundle || matchesDescription) && matchesTunnelId
        })
    }

    private func loadAllManagers() async throws -> [NETunnelProviderManager] {
        try await withCheckedThrowingContinuation { continuation in
            NETunnelProviderManager.loadAllFromPreferences { managers, error in
                if let error {
                    continuation.resume(throwing: TunnelBridgeError.managerUnavailable("Failed to load tunnel managers: \(error.localizedDescription)"))
                    return
                }
                continuation.resume(returning: managers ?? [])
            }
        }
    }

    private func save(manager: NETunnelProviderManager) async throws {
        try await withCheckedThrowingContinuation { (continuation: CheckedContinuation<Void, Error>) in
            manager.saveToPreferences { error in
                if let error {
                    continuation.resume(throwing: TunnelBridgeError.managerSaveFailed("Failed to save tunnel manager: \(error.localizedDescription)"))
                    return
                }
                continuation.resume()
            }
        }
    }

    private func load(manager: NETunnelProviderManager) async throws {
        try await withCheckedThrowingContinuation { (continuation: CheckedContinuation<Void, Error>) in
            manager.loadFromPreferences { error in
                if let error {
                    continuation.resume(throwing: TunnelBridgeError.managerUnavailable("Failed to reload tunnel manager: \(error.localizedDescription)"))
                    return
                }
                continuation.resume()
            }
        }
    }

    private func isRunning(_ status: NEVPNStatus) -> Bool {
        switch status {
        case .connected, .connecting, .reasserting:
            return true
        default:
            return false
        }
    }

    private func connectionState(_ status: NEVPNStatus) -> String {
        switch status {
        case .invalid:
            return "invalid"
        case .disconnected:
            return "disconnected"
        case .connecting:
            return "connecting"
        case .connected:
            return "connected"
        case .reasserting:
            return "reasserting"
        case .disconnecting:
            return "disconnecting"
        @unknown default:
            return "unknown"
        }
    }
}
