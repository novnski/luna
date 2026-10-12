import CryptoKit
import Foundation
import Security

enum PinnedTrust {
    struct Decision {
        let accepted: Bool
        let presented: String?
        let detail: String
    }

    /// DER prefix of a P-256 SubjectPublicKeyInfo; the raw key follows it.
    private static let p256SpkiPrefix = Data([
        0x30, 0x59, 0x30, 0x13, 0x06, 0x07, 0x2a, 0x86, 0x48, 0xce, 0x3d, 0x02, 0x01,
        0x06, 0x08, 0x2a, 0x86, 0x48, 0xce, 0x3d, 0x03, 0x01, 0x07, 0x03, 0x42, 0x00,
    ])

    static func evaluate(trust: SecTrust, challengeHost: String, expectedHost: String, pin: String) -> Decision {
        guard challengeHost.caseInsensitiveCompare(expectedHost) == .orderedSame else {
            return Decision(accepted: false, presented: nil, detail: "challenge for \(challengeHost), expected \(expectedHost)")
        }
        guard let chain = SecTrustCopyCertificateChain(trust) as? [SecCertificate], let leaf = chain.first else {
            return Decision(accepted: false, presented: nil, detail: "no server certificate")
        }
        guard let key = SecCertificateCopyKey(leaf),
              let attributes = SecKeyCopyAttributes(key) as? [CFString: Any],
              attributes[kSecAttrKeyType] as? String == kSecAttrKeyTypeECSECPrimeRandom as String,
              attributes[kSecAttrKeySizeInBits] as? Int == 256,
              let raw = SecKeyCopyExternalRepresentation(key, nil) as Data?
        else {
            return Decision(accepted: false, presented: nil, detail: "server key is not ECDSA P-256")
        }
        let presented = SHA256.hash(data: p256SpkiPrefix + raw).map { String(format: "%02x", $0) }.joined()
        guard constantTimeEqual(presented, pin) else {
            return Decision(accepted: false, presented: presented, detail: "server key does not match the pin")
        }
        // The pinned leaf is the only anchor: it must be a valid, in-date
        // certificate signed by the pinned key itself.
        SecTrustSetPolicies(trust, SecPolicyCreateBasicX509())
        SecTrustSetAnchorCertificates(trust, [leaf] as CFArray)
        SecTrustSetAnchorCertificatesOnly(trust, true)
        var error: CFError?
        guard SecTrustEvaluateWithError(trust, &error) else {
            let reason = error.map { CFErrorCopyDescription($0) as String } ?? "unknown"
            return Decision(accepted: false, presented: presented, detail: "pinned certificate is not valid: \(reason)")
        }
        return Decision(accepted: true, presented: presented, detail: "pinned key and valid certificate")
    }

    private static func constantTimeEqual(_ a: String, _ b: String) -> Bool {
        let (x, y) = (Array(a.utf8), Array(b.utf8))
        guard x.count == y.count else { return false }
        return zip(x, y).reduce(0) { $0 | ($1.0 ^ $1.1) } == 0
    }

}
