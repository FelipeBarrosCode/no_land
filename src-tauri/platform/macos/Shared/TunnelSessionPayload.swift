import Foundation

public struct TunnelSessionPayload: Codable {
    public let tunnelId: String
    public let instanceId: UInt64?
    public let interfaceName: String
    public let clientTunnelIp: String
    public let serverTunnelIp: String
    public let clientPublicKey: String
    public let serverPublicKey: String
    public let endpointHost: String
    public let endpointPort: UInt16
    public let allowedIps: String
    public let mtu: UInt16
    public let persistentKeepaliveSecs: UInt16
    public let sunshineHost: String
    public let sunshinePort: UInt16
    public let clientPrivateKey: String

    public var endpointAddress: String {
        "\(endpointHost):\(endpointPort)"
    }

    public var providerConfiguration: [String: NSObject] {
        [
            SharedConstants.Keys.tunnelId: tunnelId as NSString,
            SharedConstants.Keys.instanceId: NSNumber(value: instanceId ?? 0),
            SharedConstants.Keys.interfaceName: interfaceName as NSString,
            SharedConstants.Keys.clientTunnelIp: clientTunnelIp as NSString,
            SharedConstants.Keys.serverTunnelIp: serverTunnelIp as NSString,
            SharedConstants.Keys.clientPublicKey: clientPublicKey as NSString,
            SharedConstants.Keys.serverPublicKey: serverPublicKey as NSString,
            SharedConstants.Keys.endpointHost: endpointHost as NSString,
            SharedConstants.Keys.endpointPort: NSNumber(value: endpointPort),
            SharedConstants.Keys.allowedIps: allowedIps as NSString,
            SharedConstants.Keys.mtu: NSNumber(value: mtu),
            SharedConstants.Keys.keepalive: NSNumber(value: persistentKeepaliveSecs),
            SharedConstants.Keys.sunshineHost: sunshineHost as NSString,
            SharedConstants.Keys.sunshinePort: NSNumber(value: sunshinePort),
            SharedConstants.Keys.clientPrivateKey: clientPrivateKey as NSString,
        ]
    }

    public static func fromProviderConfiguration(_ configuration: [String: Any]) throws -> TunnelSessionPayload {
        guard
            let tunnelId = configuration[SharedConstants.Keys.tunnelId] as? String,
            let interfaceName = configuration[SharedConstants.Keys.interfaceName] as? String,
            let clientTunnelIp = configuration[SharedConstants.Keys.clientTunnelIp] as? String,
            let serverTunnelIp = configuration[SharedConstants.Keys.serverTunnelIp] as? String,
            let clientPublicKey = configuration[SharedConstants.Keys.clientPublicKey] as? String,
            let serverPublicKey = configuration[SharedConstants.Keys.serverPublicKey] as? String,
            let endpointHost = configuration[SharedConstants.Keys.endpointHost] as? String,
            let allowedIps = configuration[SharedConstants.Keys.allowedIps] as? String,
            let sunshineHost = configuration[SharedConstants.Keys.sunshineHost] as? String,
            let clientPrivateKey = configuration[SharedConstants.Keys.clientPrivateKey] as? String
        else {
            throw TunnelBridgeError.invalidRequest("Missing one or more provider configuration fields")
        }

        let endpointPort = (configuration[SharedConstants.Keys.endpointPort] as? NSNumber)?.uint16Value ?? 0
        let mtu = (configuration[SharedConstants.Keys.mtu] as? NSNumber)?.uint16Value ?? 1280
        let keepalive = (configuration[SharedConstants.Keys.keepalive] as? NSNumber)?.uint16Value ?? 25
        let sunshinePort = (configuration[SharedConstants.Keys.sunshinePort] as? NSNumber)?.uint16Value ?? 47990
        let instanceIdValue = configuration[SharedConstants.Keys.instanceId] as? NSNumber

        return TunnelSessionPayload(
            tunnelId: tunnelId,
            instanceId: instanceIdValue?.uint64Value,
            interfaceName: interfaceName,
            clientTunnelIp: clientTunnelIp,
            serverTunnelIp: serverTunnelIp,
            clientPublicKey: clientPublicKey,
            serverPublicKey: serverPublicKey,
            endpointHost: endpointHost,
            endpointPort: endpointPort,
            allowedIps: allowedIps,
            mtu: mtu,
            persistentKeepaliveSecs: keepalive,
            sunshineHost: sunshineHost,
            sunshinePort: sunshinePort,
            clientPrivateKey: clientPrivateKey
        )
    }
}

public enum SharedConstants {
    public enum Keys {
        public static let tunnelId = "tunnelId"
        public static let instanceId = "instanceId"
        public static let interfaceName = "interfaceName"
        public static let clientTunnelIp = "clientTunnelIp"
        public static let serverTunnelIp = "serverTunnelIp"
        public static let clientPublicKey = "clientPublicKey"
        public static let serverPublicKey = "serverPublicKey"
        public static let endpointHost = "endpointHost"
        public static let endpointPort = "endpointPort"
        public static let allowedIps = "allowedIps"
        public static let mtu = "mtu"
        public static let keepalive = "persistentKeepaliveSecs"
        public static let sunshineHost = "sunshineHost"
        public static let sunshinePort = "sunshinePort"
        public static let clientPrivateKey = "clientPrivateKey"
    }

    public static let managerDescription = "Noland Managed Tunnel"
    public static let providerBundleIdentifier = "com.noland.connect.tunnel"
}
