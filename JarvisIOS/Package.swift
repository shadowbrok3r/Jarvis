// swift-tools-version: 6.2

import PackageDescription
import Foundation

let root = FileManager.default.currentDirectoryPath

let package = Package(
  name: "JarvisIOS",
  platforms: [.iOS(.v17)],
  products: [
    // xtool bundles this library into an iOS app (do not use .executable here).
    .library(name: "JarvisIOS", targets: ["JarvisIOS"]),
  ],
  targets: [
    .target(
      name: "BridgeFFI",
      path: "Sources/BridgeFFI",
      publicHeadersPath: "include"
    ),
    .target(
      name: "JarvisIOS",
      dependencies: ["BridgeFFI"],
      path: "Sources/JarvisIOS",
      linkerSettings: [
        .unsafeFlags(["-L", "\(root)/RustLibs"]),
        .linkedLibrary("jarvis_ios"),
      ]
    ),
  ]
)
