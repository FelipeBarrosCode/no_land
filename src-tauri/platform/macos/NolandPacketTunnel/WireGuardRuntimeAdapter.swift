import Foundation
import Network
import NetworkExtension

#if canImport(WireGuardKit)
import WireGuardKit
#endif

protocol WireGuardRuntimeAdapter {
    func start(session: TunnelSessionPayload, provider: NEPacketTunnelProvider, completionHandler: @escaping (Error?) -> Void) throws
    func stop(completionHandler: @escaping () -> Void)
}

enum WireGuardRuntimeAdapterFactory {
    static func makeAdapter() -> WireGuardRuntimeAdapter {
        #if canImport(WireGuardKit)
        return WireGuardKitRuntimeAdapter()
        #else
        return PlaceholderRuntimeAdapter()
        #endif
    }
}

private final class PlaceholderRuntimeAdapter: WireGuardRuntimeAdapter {
    func start(session: TunnelSessionPayload, provider: NEPacketTunnelProvider, completionHandler: @escaping (Error?) -> Void) throws {
        completionHandler(TunnelBridgeError.managerUnavailable(
            "WireGuardKit is not linked into the Packet Tunnel Provider target yet"
        ))
    }

    func stop(completionHandler: @escaping () -> Void) {
        completionHandler()
    }
}

#if canImport(WireGuardKit)
private final class WireGuardKitRuntimeAdapter: WireGuardRuntimeAdapter {
    private var adapter: WireGuardAdapter?

    func start(session: TunnelSessionPayload, provider: NEPacketTunnelProvider, completionHandler: @escaping (Error?) -> Void) throws {
        let tunnelConfiguration = try makeTunnelConfiguration(session: session)
        let adapter = WireGuardAdapter(with: provider) { _, message in
            NSLog("[NolandPacketTunnel] %@", message)
        }
        self.adapter = adapter

        adapter.start(tunnelConfiguration: tunnelConfiguration) { [weak self] adapterError in
            if let adapterError {
                self?.adapter = nil
                completionHandler(Self.mapAdapterError(adapterError))
                return
            }
            completionHandler(nil)
        }
    }

    func stop(completionHandler: @escaping () -> Void) {
        guard let adapter else {
            completionHandler()
            return
        }

        adapter.stop { [weak self] _ in
            self?.adapter = nil
            completionHandler()
        }
    }

    private func makeTunnelConfiguration(session: TunnelSessionPayload) throws -> TunnelConfiguration {
        guard let privateKey = PrivateKey(base64Key: session.clientPrivateKey) else {
            throw TunnelBridgeError.invalidRequest("Invalid client private key in tunnel session payload")
        }
        guard let publicKey = PublicKey(base64Key: session.serverPublicKey) else {
            throw TunnelBridgeError.invalidRequest("Invalid server public key in tunnel session payload")
        }
        guard let endpoint = Endpoint(from: session.endpointAddress) else {
            throw TunnelBridgeError.invalidRequest("Invalid endpoint in tunnel session payload: \(session.endpointAddress)")
        }

        var interface = InterfaceConfiguration(privateKey: privateKey)
        guard let clientAddress = IPAddressRange(from: "\(session.clientTunnelIp)/32") else {
            throw TunnelBridgeError.invalidRequest("Invalid client tunnel IP in tunnel session payload")
        }
        interface.addresses = [clientAddress]
        interface.mtu = session.mtu

        var peer = PeerConfiguration(publicKey: publicKey)
        peer.endpoint = endpoint
        peer.persistentKeepAlive = session.persistentKeepaliveSecs
        peer.allowedIPs = try session.allowedIps
            .split(separator: ",")
            .map { $0.trimmingCharacters(in: .whitespacesAndNewlines) }
            .map { value in
                guard let range = IPAddressRange(from: value) else {
                    throw TunnelBridgeError.invalidRequest("Invalid AllowedIPs entry in tunnel session payload: \(value)")
                }
                return range
            }

        return TunnelConfiguration(
            name: session.interfaceName,
            interface: interface,
            peers: [peer]
        )
    }

    private static func mapAdapterError(_ error: WireGuardAdapterError) -> Error {
        switch error {
        case .cannotLocateTunnelFileDescriptor:
            return TunnelBridgeError.tunnelStartFailed("WireGuardKit could not determine the utun file descriptor")
        case .invalidState:
            return TunnelBridgeError.tunnelStartFailed("WireGuardKit adapter entered an invalid state")
        case .dnsResolution(let errors):
            let hosts = errors.map(\ .address).joined(separator: ", ")
            return TunnelBridgeError.tunnelStartFailed("WireGuardKit DNS resolution failed for: \(hosts)")
        case .setNetworkSettings(let error):
            return TunnelBridgeError.tunnelStartFailed("WireGuardKit could not apply network settings: \(error.localizedDescription)")
        case .startWireGuardBackend(let code):
            return TunnelBridgeError.tunnelStartFailed("WireGuardKit backend failed to start (wgTurnOn=\(code))")
        }
    }
}
#endif
