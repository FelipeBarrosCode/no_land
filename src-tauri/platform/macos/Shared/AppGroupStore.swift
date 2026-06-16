import Foundation

public enum AppGroupStore {
    public static let groupIdentifier = "group.com.noland.connect.shared"
    public static let statusFileName = "wireguard-status.json"

    public static func statusFileURL() -> URL? {
        FileManager.default
            .containerURL(forSecurityApplicationGroupIdentifier: groupIdentifier)?
            .appendingPathComponent(statusFileName)
    }

    public static func writeStatus(_ status: TunnelStatusPayload) {
        guard let url = statusFileURL() else { return }
        do {
            let data = try JSONEncoder().encode(status)
            try data.write(to: url, options: .atomic)
        } catch {
            // Best-effort status persistence only.
        }
    }

    public static func readStatus() -> TunnelStatusPayload? {
        guard let url = statusFileURL(),
              let data = try? Data(contentsOf: url) else {
            return nil
        }
        return try? JSONDecoder().decode(TunnelStatusPayload.self, from: data)
    }
}
