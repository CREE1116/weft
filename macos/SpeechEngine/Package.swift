// swift-tools-version: 5.9
import PackageDescription

let package = Package(
    name: "WeftSpeech",
    platforms: [.macOS(.v13)],
    dependencies: [.package(url: "https://github.com/k2-fsa/sherpa-onnx", exact: "1.13.8")],
    targets: [.executableTarget(name: "weft-speech", dependencies: [.product(name: "sherpa-onnx", package: "sherpa-onnx")])]
)
