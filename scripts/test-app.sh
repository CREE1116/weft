#!/bin/bash
# Native smoke checks against isolated preferences and generated temporary files.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
TEST_ROOT="$(mktemp -d /tmp/weft-app-check.XXXXXX)"
trap 'rm -rf "$TEST_ROOT"' EXIT
APP="$TEST_ROOT/Weft.app"
mkdir -p "$APP/Contents/MacOS"
cp -R "$ROOT/dist/Weft.app/Contents/Resources" "$APP/Contents/Resources"
cp -R "$ROOT/macos/Resources/"*.lproj "$APP/Contents/Resources/"
cp "$ROOT/macos/Info.plist" "$APP/Contents/Info.plist"
/usr/libexec/PlistBuddy -c "Set :CFBundleIdentifier local.weft.checks.$(uuidgen)" "$APP/Contents/Info.plist"
python3 - "$ROOT/macos/Weft.swift" "$TEST_ROOT/main.swift" <<'PY'
from pathlib import Path
import sys
source=Path(sys.argv[1]).read_text()
source=source[:source.index('\nlet app = NSApplication.shared')]
Path(sys.argv[2]).write_text(source)
PY
cat >> "$TEST_ROOT/main.swift" <<'SWIFT'

let testRoot = Bundle.main.bundleURL.deletingLastPathComponent().appendingPathComponent("fixtures")
try FileManager.default.createDirectory(at: testRoot, withIntermediateDirectories: true)
setenv("WEFT_DESKTOP_NO_WARMUP", "1", 1)
setenv("WEFT_DESKTOP_DB", testRoot.appendingPathComponent("not-created.sqlite").path, 1)
let app = NSApplication.shared
let delegate = AppDelegate()
app.delegate = delegate
delegate.makePanel()
delegate.makeMenu()
let ordinaryArgs = delegate.semanticArguments(text: "retry policy", level: "file")!
precondition(ordinaryArgs.contains("search") && ordinaryArgs.contains("hybrid"), "ordinary file queries should use content and meaning fusion")
let negativeArgs = delegate.semanticArguments(text: "retry - travel", level: "file")!
precondition(negativeArgs.contains("retrieve") && negativeArgs.contains("--avoid"), "negative queries must retain explicit retrieval semantics")
let wqlArgs = delegate.semanticArguments(text: "avoid(\"retry\",\"travel\")", level: "file")!
precondition(wqlArgs.contains("q") && !wqlArgs.contains("hybrid"), "WQL must retain its scoring semantics")
precondition(delegate.semanticArguments(text: "retry policy", level: "folder")!.contains("retrieve"), "folder queries must retain equal-file aggregation")
precondition(!delegate.panel.isMovableByWindowBackground, "the search panel should not drag accidentally")
delegate.setRecordingChrome(true)
precondition(delegate.panel.isMovableByWindowBackground, "the recording panel should be draggable by its background")
delegate.movePanelForRecording(true)
let recordingFrame = delegate.panelMotion!.target
precondition(recordingFrame.height == 64 && recordingFrame.width <= 244, "the recording panel should reserve enough room for its completion button")
if let visible = delegate.panel.screen?.visibleFrame ?? NSScreen.main?.visibleFrame {
    precondition(recordingFrame.maxX <= visible.maxX - 10 && recordingFrame.minX >= visible.minX + 10, "the recording panel should stay within the visible screen bounds")
}
delegate.setRecordingChrome(false)
precondition(delegate.indexToggleMenuItem?.title == "인덱싱 감시 시작", "the menu bar menu should expose an indexing start action")
precondition(!delegate.applicationShouldTerminateAfterLastWindowClosed(app), "closing app windows must leave the menu bar app alive")
precondition(delegate.isMicrophonePermissionError(VoiceCapture.CaptureError.permissionDenied), "microphone permission denial must be recognized for Settings guidance")
let setup = OnboardingController(app: delegate)
let deadline = Date().addingTimeInterval(15)
while setup.validating && Date() < deadline { RunLoop.main.run(until: Date().addingTimeInterval(0.05)) }
precondition(setup.modelReady, "installed model must validate without indexing")
precondition(setup.step == 0 && setup.next.isEnabled, "language selection must be the first page and must not wait for model setup")
let catalogSamples: [(AppLanguage, String, String)] = [
    (.ko, "Weft에 오신 것을 환영합니다", "파일명"),
    (.en, "Welcome to Weft", "Filename"),
    (.zhHans, "欢迎使用 Weft", AppLanguage.localize("파일명", language: .zhHans)),
    (.ja, "Weft へようこそ", AppLanguage.localize("파일명", language: .ja))
]
UserDefaults.standard.removeObject(forKey: AppLanguage.preferenceKey)
for (language, welcome, filename) in catalogSamples {
    setup.pendingLanguage = language
    setup.step = 0
    setup.render()
    precondition(setup.titleLabel.stringValue == welcome, "welcome must preview the selected language")
    precondition(UserDefaults.standard.string(forKey: AppLanguage.preferenceKey) == nil, "previewing a language must not commit the choice before Next")
    if language != .ko {
        precondition(AppLanguage.localize("파일명", language: language) != "파일명", "catalog must translate visible search labels")
        precondition(AppLanguage.localize("일반", language: language) != "일반", "catalog must translate settings")
    }
    precondition(!filename.isEmpty)
}
setup.pendingLanguage = .en
setup.advance()
precondition(AppLanguage.current == .en && setup.step == 1, "Next must persist the choice before model setup")
delegate.refreshLanguage()
precondition(delegate.mode.label(forSegment: 0) == "Filename")
precondition(delegate.indexProcess == nil && !FileManager.default.fileExists(atPath: delegate.db.path), "language changes must never trigger indexing")
precondition(formatLocalizedTemplate("{1} / {0}", arguments: ["first {1}", "second"]) == "second / first {1}", "formatting must allow argument reorder without touching inserted user text")
AppLanguage.select(.ko)
delegate.refreshLanguage()
setup.step = 0; setup.pendingLanguage = .ko; setup.render()
UserDefaults.standard.removeObject(forKey: AppLanguage.preferenceKey)
setup.step = 2; setup.render()
precondition(!setup.next.isEnabled, "folder is required before continuing")
delegate.folder = testRoot.path; setup.render()
precondition(setup.next.isEnabled)
setup.step = 3; setup.render()
precondition(delegate.shortcutButton?.title == "⌥Space")
setup.step = 4; setup.render()
precondition(setup.indexAfterSetup.state == .off, "setup must never index by default")
precondition(delegate.indexProcess == nil && !FileManager.default.fileExists(atPath: delegate.db.path))

delegate.updateMenuBarIndexProgress(running: false)
precondition(delegate.indexRemainingLabel?.stringValue == L("Set up indexing"), "a missing index needs a setup hint")
precondition(delegate.indexStatusContainer != nil, "the index status remains visible when no scan is running")
if case .missing = AppDelegate.indexedDocumentCount(at: delegate.db) {} else { preconditionFailure("the status probe must not create an index") }
var testDatabase: OpaquePointer?
precondition(sqlite3_open(delegate.db.path, &testDatabase) == SQLITE_OK)
precondition(sqlite3_exec(testDatabase, "CREATE TABLE documents (id INTEGER PRIMARY KEY)", nil, nil, nil) == SQLITE_OK)
delegate.refreshMenuBarIndexProgress()
precondition(delegate.indexRemainingLabel?.stringValue == LF("{0} searchable", "0"), "empty SQLite index must report zero registered files")
precondition(delegate.indexRegisteredLabel?.stringValue == L("Ready to search"))
precondition(sqlite3_exec(testDatabase, "INSERT INTO documents (id) VALUES (1)", nil, nil, nil) == SQLITE_OK)
delegate.refreshMenuBarIndexProgress()
precondition(delegate.indexRemainingLabel?.stringValue == LF("{0} searchable", "1"), "registered document count must come from SQLite")
precondition(delegate.indexRemainingLabel?.attributedStringValue.attribute(.foregroundColor, at: 0, effectiveRange: nil) as? NSColor == .systemGreen, "available file count should be green")
precondition(sqlite3_exec(testDatabase, "INSERT INTO documents (id) VALUES (2),(3)", nil, nil, nil) == SQLITE_OK)
delegate.refreshMenuBarIndexProgress()
precondition(delegate.indexRemainingLabel?.stringValue == LF("{0} searchable", "3"), "counts must refresh without loading a model")
precondition(sqlite3_close(testDatabase) == SQLITE_OK)
let brokenIndex = testRoot.appendingPathComponent("broken.sqlite")
try Data("not a database".utf8).write(to: brokenIndex)
if case .unavailable = AppDelegate.indexedDocumentCount(at: brokenIndex) {} else { preconditionFailure("corrupted indexes must not masquerade as empty indexes") }

let progressLog = delegate.db.deletingLastPathComponent().appendingPathComponent("desktop-index.log")
try "scanned=125 indexed=36 unchanged/skipped=89 errors=0\n".write(to: progressLog, atomically: true, encoding: .utf8)
delegate.indexLogStartOffset = 0
UserDefaults.standard.removeObject(forKey: "indexProgressTarget")
UserDefaults.standard.removeObject(forKey: "indexProgressTargetScope")
UserDefaults.standard.set("watch", forKey: "indexMode")
precondition(delegate.indexLogProgressSummary() == "Indexing · 125 scanned · 36 updated · 89 skipped · 0 errors", "detailed counts must stay available")
delegate.updateMenuBarIndexProgress(running: true)
precondition(delegate.statusItem.button?.isBordered == false, "the live index status should not add a bordered button bezel")
precondition(delegate.indexRemainingLabel?.stringValue == LF("{0} searchable", "3"), "the primary line must show all registered documents, not this scan's updated count")
precondition(delegate.indexRegisteredLabel?.stringValue == LF("{0} scanned", "125"), "unknown totals must show scanned files without invented estimates")
precondition(delegate.indexProgressBar?.isHidden == true && (delegate.indexProgressBar?.progress ?? -1) == 0, "hide progress until a completed scan provides a usable target")
precondition(delegate.statusItem.button?.toolTip?.contains("Total unknown") == true)
precondition(delegate.statusItem.button?.toolTip?.contains("25%") == true, "tooltip should explain the configured duty cycle")
precondition(delegate.indexingMenuItem?.title.contains("36 updated · 89 skipped · 0 errors") == true, "menu must retain full counters")
for label in [delegate.indexRemainingLabel!, delegate.indexRegisteredLabel!] {
    precondition((label.attributedStringValue.attribute(.font, at: 0, effectiveRange: nil) as? NSFont)?.pointSize == 10, "both lines must use a legible font")
}
UserDefaults.standard.set(250, forKey: "indexProgressTarget")
delegate.refreshMenuBarIndexProgress()
precondition(delegate.indexRegisteredLabel?.stringValue == LF("{0} scanned", "125"), "unscoped legacy totals must not become estimates")
UserDefaults.standard.set(delegate.indexProgressScope, forKey: "indexProgressTargetScope")
delegate.refreshMenuBarIndexProgress()
precondition(delegate.indexRegisteredLabel?.stringValue == LF("{0} / ~{1} scanned", "125", "250"), "show scanned files and an explicitly approximate total")
precondition(delegate.indexProgressBar?.progress == 0.5 && delegate.indexProgressBar?.isHidden == false)
precondition(delegate.statusItem.button?.toolTip?.contains("previous completed scan") == true)
let compactStatusWidth = delegate.statusItem.length
try "scanned=1 indexed=0 unchanged/skipped=1 errors=0\n".write(to: progressLog, atomically: true, encoding: .utf8)
UserDefaults.standard.set(987654321, forKey: "indexProgressTarget")
delegate.refreshMenuBarIndexProgress()
precondition(delegate.statusItem.length > compactStatusWidth && delegate.statusItem.length <= 180, "adapt to digits while bounding menu bar space")
delegate.refreshMenuBarIndexProgress(maximumWidth: 105)
RunLoop.main.run(until: Date().addingTimeInterval(0.05))
delegate.statusItem.button?.window?.contentView?.layoutSubtreeIfNeeded()
delegate.statusItem.button?.layoutSubtreeIfNeeded()
precondition(delegate.statusItem.length <= 105 && delegate.indexStatusDetailsWidthConstraint!.constant <= 80, "a constrained status item must respect its available width")
for label in [delegate.indexRemainingLabel!, delegate.indexRegisteredLabel!] {
    // AppKit lays out the alignment rect; a label's frame includes two-point cell insets on each side.
    let textColumn = label.alignmentRect(forFrame: label.frame)
    precondition(label.lineBreakMode == .byTruncatingTail && textColumn.width <= 80, "constrained labels must truncate inside their visible column")
    precondition(label.frame.height >= 10, "both lines must remain tall enough for their font")
}
precondition(delegate.statusItem.button?.toolTip?.contains("987,654,321") == true, "full estimated total must remain accessible when text is constrained")

func checkIndexStatus(_ line: String, secondary: String) throws {
    try (line + "\n").write(to: progressLog, atomically: true, encoding: .utf8)
    delegate.refreshMenuBarIndexProgress()
    precondition(delegate.indexRemainingLabel?.stringValue == LF("{0} searchable", "3"))
    precondition(delegate.indexRegisteredLabel?.stringValue == secondary)
}
UserDefaults.standard.set(250, forKey: "indexProgressTarget")
try checkIndexStatus("scanned=0 indexed=0 unchanged/skipped=0 errors=0", secondary: LF("{0} / ~{1} scanned", "0", "250"))
precondition(delegate.indexProgressBar?.progress == 0)
try checkIndexStatus("scanned=250 indexed=0 unchanged/skipped=250 errors=0", secondary: LF("{0} / ~{1} scanned", "250", "250"))
precondition(delegate.indexProgressBar?.progress == 0.99, "reaching an estimated target must never claim completion")
try checkIndexStatus("scanned=300 indexed=50 unchanged/skipped=250 errors=0", secondary: LF("{0} / ~{1} scanned", "300", "250"))
precondition(UserDefaults.standard.integer(forKey: "indexProgressTarget") == 250, "partial scans cannot update the completed target")
try checkIndexStatus("scanned=125 indexed=35 unchanged/skipped=89 errors=1", secondary: "1 failed")
precondition(delegate.statusItem.button?.toolTip?.contains("Failed files: 1") == true)
precondition(delegate.indexingMenuItem?.title.contains("1 errors") == true)
try checkIndexStatus("scan_complete=true scanned=125 indexed=36 unchanged/skipped=89 errors=0", secondary: LF("{0} scanned", "125"))
precondition(delegate.indexProgressBar?.progress == 1)
precondition(UserDefaults.standard.integer(forKey: "indexProgressTarget") == 125, "a newer completed scan must replace even a larger old target")
try checkIndexStatus("scan_complete=true scanned=125 indexed=35 unchanged/skipped=89 errors=1", secondary: LF("{0} failed", "1"))
precondition(delegate.statusItem.button?.toolTip?.contains("Up to date") == false && delegate.indexingMenuItem?.title.contains("Up to date") == false, "completion with failures must not claim up-to-date status")
UserDefaults.standard.set("manual", forKey: "indexMode")
try checkIndexStatus("scan_complete=true scanned=125 indexed=36 unchanged/skipped=89 errors=0", secondary: LF("{0} scanned", "125"))
UserDefaults.standard.set("watch", forKey: "indexMode")
try checkIndexStatus("scan_complete=true scanned=0 indexed=0 unchanged/skipped=0 errors=0", secondary: LF("{0} scanned", "0"))
try checkIndexStatus("scanned=0 indexed=0 unchanged/skipped=0 errors=0", secondary: LF("{0} scanned", "0"))
precondition(delegate.indexProgressBar?.isHidden == true, "an empty completed scan provides no useful future denominator")
UserDefaults.standard.set(250, forKey: "indexProgressTarget")
let originalProgressFolder = delegate.folder
delegate.folder = testRoot.appendingPathComponent("another-folder").path
try checkIndexStatus("scanned=125 indexed=36 unchanged/skipped=89 errors=0", secondary: LF("{0} scanned", "125"))
precondition(delegate.indexProgressBar?.isHidden == true, "another folder cannot reuse the completed scan estimate")
delegate.folder = originalProgressFolder
let originalProgressTypes = delegate.indexTypes
UserDefaults.standard.set(["text"], forKey: "indexTypes")
delegate.refreshMenuBarIndexProgress()
precondition(delegate.indexRegisteredLabel?.stringValue == LF("{0} scanned", "125"), "changed file types invalidate the estimate")
UserDefaults.standard.set(originalProgressTypes, forKey: "indexTypes")
let progressLogHandle = try FileHandle(forReadingFrom: progressLog)
delegate.indexLogStartOffset = try progressLogHandle.seekToEnd()
try progressLogHandle.close()
delegate.refreshMenuBarIndexProgress()
precondition(delegate.indexRemainingLabel?.stringValue == LF("{0} searchable", "3") && delegate.indexRegisteredLabel?.stringValue == L("Starting…"), "old log entries must not imply the new scan is already watching")
precondition(delegate.indexProgressBar?.isHidden == true)
delegate.indexLogStartOffset = 0
delegate.updateMenuBarIndexProgress(running: false)
precondition(delegate.indexStatusContainer != nil && delegate.indexRemainingLabel?.stringValue == LF("{0} searchable", "3"), "searchable count should remain visible after indexing stops")
precondition(delegate.indexRegisteredLabel?.stringValue == LF("Last scan: {0}", "250"), "idle status should show the last scoped scan estimate")
precondition(delegate.indexProgressBar?.isHidden == true, "idle status must not suggest indexing is running")
for command in ["@settings", "@设置", "@設定"] {
    delegate.search.stringValue = command
    delegate.controlTextDidChange(Notification(name: NSControl.textDidChangeNotification))
    precondition(delegate.hits.count == 1 && delegate.hits[0].path == "@설정", "localized command aliases must share the canonical action")
}
delegate.search.stringValue = "@설정"
delegate.controlTextDidChange(Notification(name: NSControl.textDidChangeNotification))
precondition(delegate.hits.count == 1 && delegate.hits[0].path == "@설정", "@설정 should resolve to a keyboard-selectable command")
delegate.table.selectRowIndexes(IndexSet(integer: 0), byExtendingSelection: false)
delegate.openSelection(reveal: false)
precondition(delegate.settingsWindow?.isVisible == true, "selecting @설정 should open the settings window")
delegate.settingsWindow?.orderOut(nil)
precondition(WeftTheme.menuImage.isTemplate, "the menu mark must remain a template icon without a progress ring")

let filePreview = CenteredFilePreviewView(
    frame: NSRect(x: 0, y: 0, width: 320, height: 180),
    url: testRoot.appendingPathComponent("missing-preview.docx"),
    fallback: NSImage(size: NSSize(width: 800, height: 1200))
)
precondition(filePreview.imageView.imageAlignment == .alignCenter && filePreview.imageView.imageScaling == .scaleProportionallyDown, "compact previews must center and fit the image without stretching")
precondition(filePreview.imageView.frame.width <= 96 && filePreview.imageView.frame.height <= 96, "an unavailable preview must keep its fallback icon compact")
for size in [NSSize(width: 320, height: 180), NSSize(width: 60, height: 50)] {
    filePreview.setFrameSize(size)
    filePreview.layout()
    let imageFrame = filePreview.imageView.frame
    precondition(imageFrame.midX == filePreview.bounds.midX && imageFrame.midY == filePreview.bounds.midY, "the preview must remain centered after resizing")
    precondition(filePreview.bounds.contains(imageFrame), "the preview image must fit even a small inspector")
}

// The memo editor delegates full Markdown parsing, rendering and selection to
// the vendored TextKit 2 engine without starting an index job.
delegate.buildMemoWindow()
let memo = delegate.memoDocument!
memo.text = "# Project title\n# Heading\n**bold** and `code`"
precondition(delegate.memoWindow?.title == "Project title", "the first line must stay the live memo title")
precondition(delegate.memoEditorView != nil, "memo must mount the native Markdown editor view")
let markdownAST = DocumentAST.parse("# Heading\n\n> quote\n\n```swift\nlet value = 1\n```\n\n|A|B|\n|---|---|\n|x|y|\n")
precondition(markdownAST.contains { if case .heading = $0 { return true }; return false }, "headings must parse")
precondition(markdownAST.contains { if case .blockquote = $0 { return true }; return false }, "block quotes must parse")
precondition(markdownAST.contains { if case .codeBlock = $0 { return true }; return false }, "fenced code blocks must parse")
precondition(markdownAST.contains { if case .table = $0 { return true }; return false }, "GFM tables must parse")

// The configured global shortcut toggles the search surface, including while
// transcription continues in the background.
delegate.panel.orderFrontRegardless()
precondition(delegate.panel.isVisible)
delegate.presentingSearch = true // simulate Escape during the activation retry window
delegate.panel.cancelOperation(nil)
precondition(!delegate.panel.isVisible, "Escape must dismiss search")
precondition(!delegate.presentingSearch, "Escape must cancel pending activation recovery")
delegate.applicationDidBecomeActive(Notification(name: NSApplication.didBecomeActiveNotification))
precondition(!delegate.panel.isVisible, "activation after Escape must not reopen search")
delegate.panel.orderFrontRegardless()
delegate.togglePanel()
precondition(!delegate.panel.isVisible, "second shortcut press must dismiss search")
delegate.panel.orderFrontRegardless()
delegate.waitingForVoiceResult = true
delegate.togglePanel()
precondition(!delegate.panel.isVisible, "search must dismiss while transcription continues")
delegate.waitingForVoiceResult = false

// Optional agent integration writes only into this generated fixture home.
setup.codex.state = .on; setup.claude.state = .on; setup.shared.state = .on
let home = testRoot.appendingPathComponent("agent-home")
setup.installSkillFiles(in: home)
precondition(setup.status.stringValue.contains("설치 완료"), setup.status.stringValue)
for directory in [".codex/skills", ".claude/skills", ".agents/skills"] {
    precondition(FileManager.default.fileExists(atPath: home.appendingPathComponent(directory + "/weft-semantic-search/SKILL.md").path))
}
let existing = home.appendingPathComponent(".codex/skills/weft-semantic-search/SKILL.md")
try "custom fixture skill".write(to: existing, atomically: true, encoding: .utf8)
setup.installSkillFiles(in: home)
let backups = try FileManager.default.contentsOfDirectory(at: home.appendingPathComponent(".local/share/myvec/skill-backups"), includingPropertiesForKeys: nil)
precondition(backups.contains { (try? String(contentsOf: $0.appendingPathComponent("SKILL.md"), encoding: .utf8)) == "custom fixture skill" })
let engine = Process(); engine.executableURL = home.appendingPathComponent(".cargo/bin/weft"); engine.arguments = ["--version"]
engine.standardOutput = FileHandle.nullDevice; engine.standardError = FileHandle.nullDevice
try engine.run(); engine.waitUntilExit(); precondition(engine.terminationStatus == 0, "installed helper signature must remain valid")

// A pending refresh retains the existing list and its expanded frame.
delegate.hits = [Hit(path: testRoot.appendingPathComponent("kept.pdf").path, kind: "text", score: 1, start_line: 0, end_line: 0)]
delegate.table.reloadData(); delegate.resizePanel(animated: false)
let expandedFrame = delegate.panel.frame
delegate.search.stringValue = "changed query"
delegate.controlTextDidChange(Notification(name: NSControl.textDidChangeNotification))
precondition(delegate.hits.count == 1 && delegate.hits[0].path.hasSuffix("kept.pdf"))
precondition(delegate.panel.frame == expandedFrame && delegate.refreshingResults)

// Cancellation cannot leave a stale filename search running or create a DB.
delegate.search.stringValue = "stale query"
delegate.controlTextDidChange(Notification(name: NSControl.textDidChangeNotification))
delegate.search.stringValue = ""
delegate.controlTextDidChange(Notification(name: NSControl.textDidChangeNotification))
RunLoop.main.run(until: Date().addingTimeInterval(0.4))
precondition(delegate.searchProcess == nil && !FileManager.default.fileExists(atPath: delegate.db.path))
precondition(delegate.panel.frame.height == 84)
let _ = delegate.indexedFileSettings()
precondition(delegate.indexConfigurationControls.count >= 10)
precondition(delegate.indexProcess == nil)
precondition(delegate.uninstallButton().title == "Weft 제거…", "the app should expose its uninstall action in settings")
let shim = testRoot.appendingPathComponent("myvec-shim")
try "#!/bin/sh\nexec \"$(dirname -- \"$0\")/weft\" \"$@\"\n".write(to: shim, atomically: true, encoding: .utf8)
precondition(AppDelegate.isWeftMyvecShim(at: shim), "only the exact Weft-created myvec shim may be removed")
try "#!/bin/sh\necho personal command\n".write(to: shim, atomically: true, encoding: .utf8)
precondition(!AppDelegate.isWeftMyvecShim(at: shim), "a custom myvec command must be preserved")
let mediaPrepare = NSButton(title: "미디어 분석 준비…", target: nil, action: nil); mediaPrepare.tag = 6
delegate.indexConfigurationControls = [mediaPrepare]
delegate.updateMediaPreparationButton(running: false, ready: true)
precondition(!mediaPrepare.isEnabled && mediaPrepare.title == "미디어 분석 준비 완료", "completed media setup must disable its preparation button")

// Custom app/URL shortcuts are persisted and matched before local index searches.
let linkID = UUID()
delegate.userShortcuts = [UserShortcut(id: linkID, title: "위키백과", alias: "wiki", kind: .url, target: "https://ko.wikipedia.org/w/index.php?search={query}")]
let shortcutHits = delegate.quickLinkHits(for: "wiki 고양이와 강아지")
precondition(shortcutHits?.count == 1 && shortcutHits?.first?.path == linkID.uuidString)
precondition(shortcutHits?.first?.context == "고양이와 강아지")
precondition(delegate.quickLinkHits(for: "wik")?.first?.path == linkID.uuidString)
let resolved = delegate.resolvedURL(for: "https://example.com/search?q={query}", query: "고양이 & 강아지")
precondition(URLComponents(url: resolved!, resolvingAgainstBaseURL: false)?.queryItems?.first?.value == "고양이 & 강아지")
_ = delegate.quickLinksSettings()
precondition(delegate.quickLinkRows?.arrangedSubviews.count == 1)

// The motion curve starts/ends exactly and never overshoots.
precondition(WindowMotion.progress(0) == 0 && WindowMotion.progress(1) == 1)
precondition(WindowMotion.progress(0.23) > 0 && WindowMotion.progress(0.23) < 0.5)
precondition(WindowMotion.progress(0.5) == 0.5)
precondition(WindowMotion.progress(0.8) > 0.5 && WindowMotion.progress(0.8) < 1)

// Release selection, numeric versions and archive path traversal guard.
precondition(AppRelease.newer("1.10.0", than: "1.9.0"))
precondition(!AppRelease.newer("1.0.0-beta", than: "1.0.0"))
precondition(!AppRelease.newer("1.0.0", than: "1.0.0"))
precondition(AppRelease.validArchivePaths("Weft.app/Contents/Info.plist\nWeft.app/Contents/MacOS/Weft\n"))
for unsafe in ["", "/etc/hosts", "Weft.app/../../outside", "unrelated.app/Contents/Info.plist"] {
    precondition(!AppRelease.validArchivePaths(unsafe))
}
let metadata = Data("""
{"tag_name":"v0.4.0","prerelease":false,"draft":false,"assets":[{"name":"Weft-0.4.0-arm64.zip","browser_download_url":"https://github.com/CREE1116/weft/releases/download/v0.4.0/Weft-0.4.0-arm64.zip","digest":"sha256:abc","size":100}]}
""".utf8)
let release = try JSONDecoder().decode(AppRelease.self, from: metadata)
precondition(release.archive(for: "arm64") != nil && release.archive(for: "x86_64") == nil)
let updater = AppUpdater(app: delegate)
let archive = Bundle.main.bundleURL.deletingLastPathComponent().appendingPathComponent("update-test.zip")
_ = try updater.run("/usr/bin/ditto", ["-c", "-k", "--keepParent", Bundle.main.bundleURL.path, archive.path])
let entries = try updater.run("/usr/bin/unzip", ["-Z1", archive.path])
precondition(AppRelease.validArchivePaths(entries))
try? FileManager.default.removeItem(at: archive)

// Finishing onboarding must install the CLI even with no agent skills selected.
setup.step = 4; setup.render()
setup.codex.state = .off; setup.claude.state = .off; setup.shared.state = .off
setup.indexAfterSetup.state = .off
UserDefaults.standard.removeObject(forKey: "onboardingCompleted")
let blockedHome = testRoot.appendingPathComponent("home-is-a-file")
try "file".write(to: blockedHome, atomically: true, encoding: .utf8)
setup.cliInstallationHome = blockedHome
setup.advance()
precondition(!UserDefaults.standard.bool(forKey: "onboardingCompleted"), "a failed CLI installation must keep onboarding incomplete")
precondition(setup.status.stringValue.contains("CLI 설치 실패"), "installation errors must be visible")

let automaticHome = testRoot.appendingPathComponent("onboarding-home")
let automaticCLI = automaticHome.appendingPathComponent(".cargo/bin/weft")
try FileManager.default.createDirectory(at: automaticCLI.deletingLastPathComponent(), withIntermediateDirectories: true)
try "previous CLI".write(to: automaticCLI, atomically: true, encoding: .utf8)
setup.cliInstallationHome = automaticHome
setup.advance()
precondition(UserDefaults.standard.bool(forKey: "onboardingCompleted"), "Finishing setup must commit only after installing the CLI")
precondition(FileManager.default.isExecutableFile(atPath: automaticCLI.path), "onboarding must install an executable CLI")
let installed = Process(); installed.executableURL = automaticCLI; installed.arguments = ["--version"]
installed.standardOutput = FileHandle.nullDevice; installed.standardError = FileHandle.nullDevice
try installed.run(); installed.waitUntilExit(); precondition(installed.terminationStatus == 0)
let cliArchive = automaticHome.appendingPathComponent(".local/share/myvec/cli-backups")
let backupsBefore = try FileManager.default.contentsOfDirectory(at: cliArchive, includingPropertiesForKeys: nil)
precondition(backupsBefore.count == 1, "an existing CLI must be backed up")
let backedUpCLI = try String(contentsOf: backupsBefore[0], encoding: .utf8)
precondition(backedUpCLI == "previous CLI", "the backup must retain the previous CLI")
try setup.installCLI(in: automaticHome)
let backupsAfter = try FileManager.default.contentsOfDirectory(at: cliArchive, includingPropertiesForKeys: nil)
precondition(backupsAfter.count == 1, "reinstalling the same CLI must not add a backup")
precondition(!FileManager.default.fileExists(atPath: automaticHome.appendingPathComponent(".agents/skills/weft-semantic-search").path), "CLI setup must not install optional agent skills")
precondition(delegate.indexProcess == nil, "finishing onboarding must not start indexing without consent")
UserDefaults.standard.removePersistentDomain(forName: Bundle.main.bundleIdentifier!)
print("PASS: setup validation + automatic CLI install and rollback-safe failure, no implicit indexing, live menu status + scoped estimates + failures + constrained width, centered compact file previews, isolated skill install + backups + CLI, retained results during refresh + cancelled queries, settings controls, release selection, archive paths")
SWIFT
MARKDOWN_SOURCES=()
while IFS= read -r -d '' source; do MARKDOWN_SOURCES+=("$source"); done < <(find "$ROOT/macos/Vendor/MarkdownEngine" -type f -name '*.swift' -print0)
swiftc -swift-version 5 -target "$(uname -m)-apple-macosx14.0" -framework Cocoa -framework Carbon -framework QuickLookUI -framework QuickLookThumbnailing -framework AVFoundation -framework SwiftUI \
  "$TEST_ROOT/main.swift" "$ROOT/macos/VoiceCapture.swift" "$ROOT/macos/Notes.swift" "$ROOT/macos/Onboarding.swift" "$ROOT/macos/IndexSettings.swift" "$ROOT/macos/QuickLinks.swift" "$ROOT/macos/Updates.swift" "$ROOT/macos/Brand.swift" "$ROOT/macos/WindowMotion.swift" "$ROOT/macos/FileThumbnailView.swift" "$ROOT/macos/AppLanguage.swift" \
  "${MARKDOWN_SOURCES[@]}" \
  -o "$APP/Contents/MacOS/Weft"
codesign --force --sign - "$APP" >/dev/null
"$APP/Contents/MacOS/Weft"
