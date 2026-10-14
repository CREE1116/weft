#!/bin/bash
# Native smoke checks against isolated preferences and generated temporary files.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
TEST_ROOT="$(mktemp -d /tmp/weft-app-check.XXXXXX)"
trap 'rm -rf "$TEST_ROOT"' EXIT
APP="$TEST_ROOT/Weft.app"
mkdir -p "$APP/Contents/MacOS"
cp -R "$ROOT/dist/Weft.app/Contents/Resources" "$APP/Contents/Resources"
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
setup.step = 1; setup.render()
precondition(!setup.next.isEnabled, "folder is required before continuing")
delegate.folder = testRoot.path; setup.render()
precondition(setup.next.isEnabled)
setup.step = 2; setup.render()
precondition(delegate.shortcutButton?.title == "⌥Space")
setup.step = 3; setup.render()
precondition(setup.indexAfterSetup.state == .off, "setup must never index by default")
precondition(delegate.indexProcess == nil && !FileManager.default.fileExists(atPath: delegate.db.path))

let progressLog = delegate.db.deletingLastPathComponent().appendingPathComponent("desktop-index.log")
try "scanned=125 indexed=36 unchanged/skipped=89 errors=0\n".write(to: progressLog, atomically: true, encoding: .utf8)
delegate.indexLogStartOffset = 0
UserDefaults.standard.removeObject(forKey: "indexProgressTarget")
precondition(delegate.indexLogProgressSummary() == "인덱싱 · 확인 125 · 반영 36 · 건너뜀 89", "menu bar progress should summarize current index counts")
delegate.updateMenuBarIndexProgress(running: true)
precondition(delegate.statusItem.button?.isBordered == false, "the live index status should not add a bordered button bezel")
delegate.refreshMenuBarIndexProgress()
precondition(delegate.indexRemainingLabel?.attributedStringValue.string == "확인  125개", "first scan should show real counts without inventing a remaining estimate")
precondition(delegate.indexRegisteredLabel?.attributedStringValue.string == "등록  36개", "menu bar should show newly registered count without a badge-style label")
precondition(delegate.indexProgressBar?.isHidden == true && (delegate.indexProgressBar?.progress ?? -1) == 0, "hide progress until a real estimate is available")
UserDefaults.standard.set(250, forKey: "indexProgressTarget")
delegate.refreshMenuBarIndexProgress()
precondition(delegate.indexRemainingLabel?.attributedStringValue.string == "남음  ~125개", "later scans should show an explicitly estimated remaining count")
precondition((delegate.indexProgressBar?.progress ?? 0) > 0 && delegate.indexProgressBar?.isHidden == false, "the bar should represent progress against the previous completed scan")
precondition(delegate.statusItem.button?.toolTip?.contains("이전 인덱스") == true, "the progress estimate should be explained in the status tooltip")
let compactStatusWidth = delegate.statusItem.length
try "scanned=125 indexed=98765 unchanged/skipped=0 errors=0\n".write(to: progressLog, atomically: true, encoding: .utf8)
delegate.refreshMenuBarIndexProgress()
precondition(delegate.statusItem.length > compactStatusWidth, "the menu bar status item should expand when the displayed counts gain digits")
delegate.updateMenuBarIndexProgress(running: false)
delegate.search.stringValue = "@설정"
delegate.controlTextDidChange(Notification(name: NSControl.textDidChangeNotification))
precondition(delegate.hits.count == 1 && delegate.hits[0].path == "@설정", "@설정 should resolve to a keyboard-selectable command")
delegate.table.selectRowIndexes(IndexSet(integer: 0), byExtendingSelection: false)
delegate.openSelection(reveal: false)
precondition(delegate.settingsWindow?.isVisible == true, "selecting @설정 should open the settings window")
delegate.settingsWindow?.orderOut(nil)
precondition(WeftTheme.menuImage.isTemplate, "the menu mark must remain a template icon without a progress ring")

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
UserDefaults.standard.removePersistentDomain(forName: Bundle.main.bundleIdentifier!)
print("PASS: setup validation, no implicit indexing, isolated skill install + backups + CLI, retained results during refresh + cancelled queries, settings controls, release selection, archive paths")
SWIFT
MARKDOWN_SOURCES=()
while IFS= read -r -d '' source; do MARKDOWN_SOURCES+=("$source"); done < <(find "$ROOT/macos/Vendor/MarkdownEngine" -type f -name '*.swift' -print0)
swiftc -swift-version 5 -target "$(uname -m)-apple-macosx14.0" -framework Cocoa -framework Carbon -framework QuickLookUI -framework AVFoundation -framework SwiftUI \
  "$TEST_ROOT/main.swift" "$ROOT/macos/VoiceCapture.swift" "$ROOT/macos/Notes.swift" "$ROOT/macos/Onboarding.swift" "$ROOT/macos/IndexSettings.swift" "$ROOT/macos/QuickLinks.swift" "$ROOT/macos/Updates.swift" "$ROOT/macos/Brand.swift" "$ROOT/macos/WindowMotion.swift" \
  "${MARKDOWN_SOURCES[@]}" \
  -o "$APP/Contents/MacOS/Weft"
codesign --force --sign - "$APP" >/dev/null
"$APP/Contents/MacOS/Weft"
