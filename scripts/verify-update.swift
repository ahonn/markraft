// Check the archive against the same public key embedded in the application.
import CryptoKit
import Foundation

func fail(_ message: String) -> Never {
    fputs("\(message)\n", stderr)
    exit(1)
}

guard CommandLine.arguments.count == 4,
      let key = Data(base64Encoded: CommandLine.arguments[2]),
      let signature = Data(base64Encoded: CommandLine.arguments[3]) else {
    fail("Usage: verify-update.swift ARCHIVE PUBLIC_KEY SIGNATURE")
}
do {
    let archive = try Data(contentsOf: URL(fileURLWithPath: CommandLine.arguments[1]))
    let publicKey = try Curve25519.Signing.PublicKey(rawRepresentation: key)
    guard publicKey.isValidSignature(signature, for: archive) else {
        fail("Update signature does not match the application's public key")
    }
    print("Update signature verified.")
} catch {
    fail("Could not verify update: \(error)")
}
