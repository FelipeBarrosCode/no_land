import Foundation
import NetworkExtension

#if canImport(WireGuardKit)
import WireGuardKit
#endif

final class PacketTunnelProvider: NEPacketTunnelProvider {
    private let runtimeAdapter = WireGuardRuntimeAdapterFactory.makeAdapter()

    override func startTunnel(options: [String : NSObject]?, completionHandler: @escaping (Error?) -> Void) {
        do {
            guard let protocolConfiguration = protocolConfiguration as? NETunnelProviderProtocol,
                  let providerConfiguration = protocolConfiguration.providerConfiguration else {
                throw TunnelBridgeError.invalidRequest("Packet tunnel provider configuration was missing")
            }

            let session = try TunnelSessionPayload.fromProviderConfiguration(providerConfiguration)

            try applyNetworkSettings(session: session) { [weak self] error in
                guard let self else {
                    completionHandler(TunnelBridgeError.managerUnavailable("Packet tunnel provider was released during startup"))
                    return
                }

                if let error {
                    AppGroupStore.writeStatus(
                        TunnelStatusPayload.error("Failed applying packet tunnel network settings: \(error.localizedDescription)")
                    )
                    completionHandler(error)
                    return
                }

                do {
                    try self.runtimeAdapter.start(session: session, provider: self) { adapterError in
                        if let adapterError {
                            AppGroupStore.writeStatus(
                                TunnelStatusPayload.error(adapterError.localizedDescription)
                            )
                            completionHandler(adapterError)
                            return
                        }

                        AppGroupStore.writeStatus(
                            TunnelStatusPayload(
                                managerInstalled: true,
                                managerEnabled: true,
                                providerRunning: true,
                                routeReady: true,
                                tunnelIp: session.clientTunnelIp,
                                sunshineReachable: false,
                                state: "connected",
                                lastError: nil
                            )
                        )
                        completionHandler(nil)
                    }
                } catch {
                    AppGroupStore.writeStatus(TunnelStatusPayload.error(error.localizedDescription))
                    completionHandler(error)
                }
            }
        } catch {
            AppGroupStore.writeStatus(TunnelStatusPayload.error(error.localizedDescription))
            completionHandler(error)
        }
    }

    override func stopTunnel(with reason: NEProviderStopReason, completionHandler: @escaping () -> Void) {
        runtimeAdapter.stop {
            AppGroupStore.writeStatus(
                TunnelStatusPayload(
                    managerInstalled: true,
                    managerEnabled: true,
                    providerRunning: false,
                    routeReady: false,
                    tunnelIp: "",
                    sunshineReachable: false,
                    state: "stopped",
                    lastError: nil
                )
            )
            completionHandler()
        }
    }

    private func applyNetworkSettings(session: TunnelSessionPayload, completionHandler: @escaping (Error?) -> Void) throws {
        let settings = NEPacketTunnelNetworkSettings(tunnelRemoteAddress: session.serverTunnelIp)
        let ipv4 = NEIPv4Settings(addresses: [session.clientTunnelIp], subnetMasks: ["255.255.255.255"])
        ipv4.includedRoutes = [NEIPv4Route(destinationAddress: session.serverTunnelIp, subnetMask: "255.255.255.255")]
        settings.ipv4Settings = ipv4
        settings.mtu = NSNumber(value: session.mtu)
        setTunnelNetworkSettings(settings, completionHandler: completionHandler)
    }
}
