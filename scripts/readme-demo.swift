// AppKit presentation harness. Uses the production AppDelegate and real CLI results.
import Cocoa

struct DemoFrame: Decodable {
    let name: String
    let query: String
    let mode: Int
    let result: String?
    let duration: Int
    let scene: Int
}
let demoRoot = URL(fileURLWithPath: CommandLine.arguments[1])
setenv("WEFT_DESKTOP_NO_WARMUP", "1", 1)
setenv("WEFT_DESKTOP_DB", demoRoot.appendingPathComponent("index.sqlite").path, 1)
let demoApp = NSApplication.shared
demoApp.setActivationPolicy(.prohibited)
let demoDelegate = AppDelegate()
demoApp.delegate = demoDelegate
demoDelegate.makePanel()
demoDelegate.panel.appearance = NSAppearance(named: .darkAqua)
demoDelegate.panelBackground.blendingMode = .withinWindow
// Render our own window off-screen. Never capture the desktop or user's files.
demoDelegate.panel.setFrameOrigin(NSPoint(x: -3000, y: -3000))
demoDelegate.panel.orderFrontRegardless()
let frames = try JSONDecoder().decode([DemoFrame].self, from: Data(contentsOf: demoRoot.appendingPathComponent("frames.json")))
for frame in frames {
    demoDelegate.search.stringValue = frame.query
    demoDelegate.mode.selectedSegment = frame.mode
    demoDelegate.search.placeholderString = frame.mode == 0 ? "파일·폴더 검색" : "내용으로 검색"
    let hits = try frame.result.map { try JSONDecoder().decode([Hit].self, from: Data(contentsOf: demoRoot.appendingPathComponent($0))) } ?? []
    demoDelegate.applyResults(hits)
    demoDelegate.panelMotion?.cancel()
    demoDelegate.resizePanel(animated: false)
    demoDelegate.modeBadge.title = frame.mode == 0 ? "파일명 ⇥" : "의미 ⇥"
    demoDelegate.hint.stringValue = "\(hits.count)개 · ↑↓ 선택·미리보기 · ⏎ 열기 · ⌥⏎ Finder · ⇥ 검색 방식"
    let deadline = Date().addingTimeInterval(frame.result == nil ? 0.025 : 0.35)
    while Date() < deadline { RunLoop.main.run(until: Date().addingTimeInterval(0.01)) }
    demoDelegate.panel.contentView!.layoutSubtreeIfNeeded()
    let view = demoDelegate.panel.contentView!
    view.displayIfNeeded()
    let rep = view.bitmapImageRepForCachingDisplay(in: view.bounds)!
    view.cacheDisplay(in: view.bounds, to: rep)
    let data = rep.representation(using: .png, properties: [:])!
    try data.write(to: demoRoot.appendingPathComponent(frame.name + ".png"))
}
// Office preview proof uses a real generated .docx and the production inspector.
let officeURL = demoRoot.appendingPathComponent("Documents/Centered preview.docx")
demoDelegate.search.stringValue = "docx"
demoDelegate.applyResults([Hit(path: officeURL.path, kind: "text", score: 1, start_line: 0, end_line: 0)])
demoDelegate.panelMotion?.cancel(); demoDelegate.resizePanel(animated: false)
let previewDeadline = Date().addingTimeInterval(10)
while Date() < previewDeadline {
    RunLoop.main.run(until: Date().addingTimeInterval(0.05))
    if let view = demoDelegate.inspectorContent.subviews.first as? CenteredFilePreviewView,
       view.imageView.frame.width > 96 { break }
}
if let view = demoDelegate.inspectorContent.subviews.first as? CenteredFilePreviewView {
    precondition(view.imageView.imageAlignment == .alignCenter)
    precondition(abs(view.imageView.frame.midX - view.bounds.midX) < 1)
    precondition(abs(view.imageView.frame.midY - view.bounds.midY) < 1)
    print("Office thumbnail generated: \(view.imageView.frame.width > 96)")
}
func captureView(_ view: NSView, name: String) throws {
    view.layoutSubtreeIfNeeded(); view.displayIfNeeded()
    let rep = view.bitmapImageRepForCachingDisplay(in: view.bounds)!
    view.cacheDisplay(in: view.bounds, to: rep)
    try rep.representation(using: .png, properties: [:])!.write(to: demoRoot.appendingPathComponent(name + ".png"))
}
try captureView(demoDelegate.panel.contentView!, name: "office-preview")
let welcome = OnboardingController(app: demoDelegate)
welcome.pendingLanguage = .en
welcome.window?.appearance = NSAppearance(named: .darkAqua)
welcome.render()
welcome.window?.setFrameOrigin(NSPoint(x: -3000,y: -3000))
welcome.window?.orderFrontRegardless()
RunLoop.main.run(until: Date().addingTimeInterval(0.3))
try captureView(welcome.window!.contentView!, name: "welcome-preview")
welcome.window?.orderOut(nil)
demoDelegate.panel.orderOut(nil)
UserDefaults.standard.removePersistentDomain(forName: Bundle.main.bundleIdentifier!)
print("Captured \(frames.count) native AppKit frames with isolated fixtures")
