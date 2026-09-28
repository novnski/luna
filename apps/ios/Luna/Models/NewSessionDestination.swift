import Foundation

/// A new session must belong to an existing project.
enum NewSessionDestination: Hashable {
    case project(spaceId: String)

    func space(in spaces: [Space]) -> Space? {
        guard case .project(let id) = self else { return nil }
        return spaces.first { $0.id == id }
    }

    func deviceId(spaces: [Space]) -> String? { space(in: spaces)?.deviceId }
}

extension DeviceRow {
    var canHostSessions: Bool { platform != "ios" && !id.isEmpty }
}
