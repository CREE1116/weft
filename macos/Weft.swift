import Cocoa
import Carbon
import QuickLookUI
import QuickLookThumbnailing
import QuartzCore
import SQLite3

enum SearchIndexAvailability {
    case missing, empty, ready, unavailable
}

enum IndexedDocumentCount {
    case missing, available(Int), unavailable
}

struct Hit: Decodable {
    let path: String
    let kind: String
    let score: Double
    let start_line: Int
    let end_line: Int
    var context: String? = nil
    var chunk_index: Int? = nil
}

struct Retrieval: Decodable { let hits: [Hit] }

final class SearchPanel: NSPanel {
    var escapeHandler: (() -> Void)?
    override var canBecomeKey: Bool { true }
    override var canBecomeMain: Bool { false }

    override func keyDown(with event: NSEvent) {
        if event.keyCode == 53 { escapeHandler?(); return }
        super.keyDown(with: event)
    }

    override func cancelOperation(_ sender: Any?) {
        escapeHandler?()
    }
}

final class ResultTable: NSTableView {
    var contextMenu: ((Int) -> NSMenu?)?
    override func menu(for event: NSEvent) -> NSMenu? {
        let index = row(at: convert(event.locationInWindow, from: nil))
        guard index >= 0 else { return nil }
        selectRowIndexes(IndexSet(integer: index), byExtendingSelection: false)
        return contextMenu?(index)
    }
}

final class IndexProgressBarView: NSView {
    var progress: CGFloat = 0 { didSet { needsDisplay = true } }

    override func draw(_ dirtyRect: NSRect) {
        let rect = bounds.insetBy(dx: 0, dy: max(0, (bounds.height - 2) / 2))
        NSColor.white.withAlphaComponent(0.24).setFill()
        NSBezierPath(roundedRect: rect, xRadius: 1, yRadius: 1).fill()
        guard progress > 0 else { return }
        var fill = rect
        fill.size.width *= min(1, max(0, progress))
        NSColor.white.withAlphaComponent(0.92).setFill()
        NSBezierPath(roundedRect: fill, xRadius: 1, yRadius: 1).fill()
    }
}

final class ResultRow: NSTableRowView {
    override func drawSelection(in dirtyRect: NSRect) {
        WeftTheme.accent.withAlphaComponent(isEmphasized ? 0.16 : 0.10).setFill()
        NSBezierPath(roundedRect: bounds.insetBy(dx: 4, dy: 2), xRadius: 10, yRadius: 10).fill()
    }
}

final class ShortcutRecorderButton: NSButton {
    weak var appDelegate: AppDelegate?
    override var acceptsFirstResponder: Bool { true }

    override func keyDown(with event: NSEvent) {
        guard let appDelegate = appDelegate, appDelegate.recordingShortcut else {
            super.keyDown(with: event)
            return
        }
        appDelegate.captureShortcut(event)
    }
}

final class AppDelegate: NSObject, NSApplicationDelegate, NSWindowDelegate,
    NSSearchFieldDelegate, NSTextViewDelegate, NSTableViewDataSource, NSTableViewDelegate {
    let db: URL = {
        if let override = ProcessInfo.processInfo.environment["WEFT_DESKTOP_DB"] {
            return URL(fileURLWithPath: override)
        }
        return FileManager.default.homeDirectoryForCurrentUser
            .appendingPathComponent(".local/share/myvec/index.sqlite")
    }()
    var daemonSocket: URL { db.deletingLastPathComponent().appendingPathComponent("desktop-embedding.sock") }
    var engine: URL { Bundle.main.resourceURL!.appendingPathComponent("weft") }
    var panel: SearchPanel!
    var panelMotion: WindowMotion?
    let panelBackground = NSVisualEffectView()
    var recordingRowCenterY: NSLayoutConstraint!
    var statusItem: NSStatusItem!
    var indexingMenuItem: NSMenuItem?
    var indexToggleMenuItem: NSMenuItem?
    var indexingStatusTimer: Timer?
    var indexLogStartOffset: UInt64 = 0
    var indexStatusContainer: NSStackView?
    var indexProgressIcon: NSImageView?
    var indexRemainingLabel: NSTextField?
    var indexRegisteredLabel: NSTextField?
    var indexProgressBar: IndexProgressBarView?
    var indexStatusDetailsWidthConstraint: NSLayoutConstraint?
    var indexStatusBarWidthConstraint: NSLayoutConstraint?
    let search = NSSearchField()
    let mode = NSSegmentedControl(labels: [L("파일명"), L("의미 검색")], trackingMode: .selectOne, target: nil, action: nil)
    let modeBadge = NSButton(title: L("파일명 ⇥"), target: nil, action: nil)
    let recordingDot = NSView()
    let recordingLabel = NSTextField(labelWithString: L("녹음 중 · 00:00"))
    let recordingStopButton = NSButton()
    let searchIconView = NSImageView(image: NSImage(systemSymbolName: "magnifyingglass", accessibilityDescription: nil)!)
    let voiceCapture = VoiceCapture()
    var voiceTimer: Timer?
    var voiceStartedAt: Date?
    var waitingForVoiceResult = false
    var voiceIndexProcess: Process?
    var memoWindow: NSWindow?
    var memoDocument: MemoDocument?
    var memoEditorView: NSView?
    var memoPathLabels: [Int: NSTextField] = [:]
    let resultScroll = NSScrollView()
    let divider = NSBox()
    var resultHeight: NSLayoutConstraint!
    var previewWindow: NSWindow?
    var quickLookView: QLPreviewView?
    let inspector = NSView()
    let inspectorIcon = NSImageView()
    let inspectorName = NSTextField(labelWithString: "")
    let inspectorSubtitle = NSTextField(labelWithString: "")
    let inspectorMetadata = NSStackView()
    let inspectorContent = NSView()
    var inspectorPreview: QLPreviewView?
    var inspectorWork: DispatchWorkItem?
    var resultTrailing: NSLayoutConstraint!
    let table = ResultTable()
    let hint = NSTextField(labelWithString: L("검색할 파일명이나 폴더를 입력하세요"))
    var hits: [Hit] = []
    var refreshingResults = false
    var replacingResults = false
    var inspectedHit: Hit?
    var inspectedStamp = ""
    var debounce: DispatchWorkItem?
    var generation = 0
    var searchProcess: Process?
    var daemonProcess: Process?
    var onboarding: OnboardingController?
    var indexProcess: Process?
    var indexTimer: Timer?
    var mediaSetupProcess: Process?
    var indexConfigurationControls: [NSControl] = []
    var updater: AppUpdater?
    var hotKey: EventHotKeyRef?
    var hotKeyHandler: EventHandlerRef?
    var languageObserver: NSObjectProtocol?
    var keyboardMonitor: Any?
    var folderPicker: NSOpenPanel?
    let agentCodex = NSButton(checkboxWithTitle: "Codex", target: nil, action: nil)
    let agentClaude = NSButton(checkboxWithTitle: "Claude Code", target: nil, action: nil)
    let agentShared = NSButton(checkboxWithTitle: L("공용 에이전트"), target: nil, action: nil)
    let agentStatus = NSTextField(wrappingLabelWithString: L("설치할 에이전트를 선택하세요."))
    var settingsWindow: NSWindow?
    var folderLabel: NSTextField?
    var indexingLabel: NSTextField?
    var indexButton: NSButton?
    var logHandle: FileHandle?
    var shortcutButton: NSButton?
    var shortcutHelp: NSTextField?
    var generalShortcutHelp: NSTextField?
    var quickLinkType: NSPopUpButton?
    var quickLinkTitle: NSTextField?
    var quickLinkAlias: NSTextField?
    var quickLinkTarget: NSTextField?
    var quickLinkRows: NSStackView?
    var quickLinkStatus: NSTextField?
    var recordingShortcut = false
    var presentingSearch = false
    var shortcutCode: UInt32 { UInt32(UserDefaults.standard.object(forKey: "shortcutCode") as? Int ?? kVK_Space) }
    var shortcutModifiers: UInt32 { UInt32(UserDefaults.standard.object(forKey: "shortcutModifiers") as? Int ?? optionKey) }
    var shortcutName: String { UserDefaults.standard.string(forKey: "shortcutName") ?? "⌥Space" }
    var folder: String {
        get { UserDefaults.standard.string(forKey: "indexFolder") ?? "" }
        set { UserDefaults.standard.set(newValue, forKey: "indexFolder") }
    }

    func applicationDidFinishLaunching(_ notification: Notification) {
        NSApp.setActivationPolicy(.accessory)
        applyAppearance()
        makePanel()
        makeMenu()
        languageObserver = NotificationCenter.default.addObserver(forName: AppLanguage.languageChangedNotification, object: nil, queue: .main) { [weak self] _ in self?.refreshLanguage() }
        registerShortcut()
        keyboardMonitor = NSEvent.addLocalMonitorForEvents(matching: .keyDown) { [weak self] event in
            guard let self = self else { return event }
            if self.recordingShortcut { return event }
            if event.window == self.previewWindow {
                if event.keyCode == 53 || (event.modifierFlags.contains(.command) && event.charactersIgnoringModifiers?.lowercased() == "y") {
                    self.closePreview(); return nil
                }
                // Space belongs to the media player here (play / pause).
            }
            if event.window == self.memoWindow, event.modifierFlags.contains(.command), event.charactersIgnoringModifiers?.lowercased() == "s" {
                self.saveMemo(); return nil
            }
            if event.window == self.settingsWindow && event.keyCode == 53 { self.settingsWindow?.close(); return nil }
            if event.keyCode == 53, self.panel.isVisible, self.panel.isKeyWindow {
                self.dismissSearchPanelForEscape(); return nil
            }
            guard event.window == self.panel else { return event }
            if event.modifierFlags.contains(.command), let key = event.charactersIgnoringModifiers?.lowercased() {
                if let index = ["1", "2", "3"].firstIndex(of: key) { self.selectScope(["file", "folder", "chunk"][index]); return nil }
                if key == "y" { self.showPreview(); return nil }
                if key == "q" { self.quit(); return nil }
                if key == "w" { self.hide(); return nil }
                if let editor = self.search.currentEditor() as? NSTextView {
                    switch key {
                    case "a": editor.selectAll(nil); return nil
                    case "c": editor.copy(nil); return nil
                    case "v": editor.paste(nil); return nil
                    case "x": editor.cut(nil); return nil
                    case "z":
                        if event.modifierFlags.contains(.shift) { editor.undoManager?.redo() }
                        else { editor.undoManager?.undo() }
                        return nil
                    default: break
                    }
                }
            }
            if let editor = self.search.currentEditor() as? NSTextView, editor.hasMarkedText() { return event }
            switch event.keyCode {
            case 53:
                self.dismissSearchPanelForEscape()
                return nil
            case 48: self.cycleMode(); return nil
            case 125: self.moveSelection(1); return nil
            case 126: self.moveSelection(-1); return nil
            case 36, 76:
                if self.refreshingResults { return nil }
                if self.mode.selectedSegment == 1 && self.hits.isEmpty { self.performSearch() }
                else { self.openSelection(reveal: event.modifierFlags.contains(.option)) }
                return nil
            default: return event
            }
        }
        if UserDefaults.standard.string(forKey: "uiLanguage") == nil || (!UserDefaults.standard.bool(forKey: "onboardingCompleted") && !FileManager.default.fileExists(atPath: db.path)) {
            showOnboarding()
        } else if UserDefaults.standard.object(forKey: "showOnLaunch") as? Bool ?? true { showPanel() }
        resumeIndexing()
        updater = AppUpdater(app: self)
        updater?.start()
    }

    func applicationShouldHandleReopen(_ sender: NSApplication, hasVisibleWindows flag: Bool) -> Bool {
        showPanel()
        return true
    }

    func applicationShouldTerminateAfterLastWindowClosed(_ sender: NSApplication) -> Bool {
        // Closing search/settings should leave the menu bar app and its indexer alive.
        false
    }

    func applicationWillTerminate(_ notification: Notification) {
        cancelSearch()
        if let recording = voiceCapture.stop() { try? FileManager.default.removeItem(at: recording) }
        voiceTimer?.invalidate()
        clearInspector()
        dismissPreview()
        if daemonProcess?.isRunning == true { daemonProcess?.terminate() }
        indexTimer?.invalidate()
        if mediaSetupProcess?.isRunning == true { mediaSetupProcess?.terminate() }
        if indexProcess?.isRunning == true { indexProcess?.terminate() }
        if let hotKey = hotKey { UnregisterEventHotKey(hotKey) }
        if let handler = hotKeyHandler { RemoveEventHandler(handler) }
        if let monitor = keyboardMonitor { NSEvent.removeMonitor(monitor) }
        try? logHandle?.close()
    }

    func makeMenu() {
        statusItem = NSStatusBar.system.statusItem(withLength: NSStatusItem.squareLength)
        statusItem.button?.image = WeftTheme.menuImage
        statusItem.button?.image?.isTemplate = true
        statusItem.button?.title = ""
        statusItem.button?.imagePosition = .imageOnly
        statusItem.button?.toolTip = L("Weft · 검색 및 설정")
        let menu = NSMenu()
        menu.addItem(withTitle: LF("Weft 검색…   {0}", String(describing: shortcutName)), action: #selector(showPanel), keyEquivalent: "")
        let indexToggle = menu.addItem(withTitle: L("인덱싱 감시 시작"), action: #selector(toggleIndexing), keyEquivalent: "")
        indexToggleMenuItem = indexToggle
        let progress = NSMenuItem(title: L("인덱싱 진행 중…"), action: nil, keyEquivalent: "")
        progress.isEnabled = false
        progress.isHidden = true
        menu.addItem(progress)
        indexingMenuItem = progress
        menu.addItem(withTitle: L("설정…"), action: #selector(showSettings), keyEquivalent: "")
        menu.addItem(withTitle: L("처음 사용 설정…"), action: #selector(showOnboarding), keyEquivalent: "")
        menu.addItem(withTitle: L("새 메모…"), action: #selector(createMemoFromMenu), keyEquivalent: "")
        menu.addItem(withTitle: L("검색 오류 로그 열기"), action: #selector(openAppLog), keyEquivalent: "")
        menu.addItem(withTitle: L("인덱싱 로그 열기"), action: #selector(openLog), keyEquivalent: "")
        menu.addItem(.separator())
        menu.addItem(withTitle: L("Weft 종료"), action: #selector(quit), keyEquivalent: "q")
        for item in menu.items { item.target = self }
        statusItem.menu = menu
        // Keep native edit/window shortcuts available in accessory windows,
        // including selectable chunk previews. Settings has no keyboard menu entry.
        let main = NSMenu()
        let applicationItem = NSMenuItem(title: "Weft", action: nil, keyEquivalent: "")
        let application = NSMenu(title: "Weft")
        let settingsItem = application.addItem(withTitle: L("설정…"), action: #selector(showSettings), keyEquivalent: "")
        settingsItem.target = self
        let setupItem = application.addItem(withTitle: L("처음 사용 설정…"), action: #selector(showOnboarding), keyEquivalent: "")
        setupItem.target = self
        application.addItem(.separator())
        let quitItem = application.addItem(withTitle: L("Weft 종료"), action: #selector(quit), keyEquivalent: "q")
        quitItem.target = self
        applicationItem.submenu = application
        main.addItem(applicationItem)
        let editItem = NSMenuItem(title: L("편집"), action: nil, keyEquivalent: "")
        let edit = NSMenu(title: L("편집"))
        for (title, selector, key) in [(L("전체 선택"), "selectAll:", "a"), (L("복사"), "copy:", "c"),
            (L("붙여넣기"), "paste:", "v"), (L("잘라내기"), "cut:", "x"), (L("실행 취소"), "undo:", "z")] {
            edit.addItem(withTitle: title, action: NSSelectorFromString(selector), keyEquivalent: key)
        }
        editItem.submenu = edit
        main.addItem(editItem)
        let windowItem = NSMenuItem(title: L("윈도우"), action: nil, keyEquivalent: "")
        let windows = NSMenu(title: L("윈도우"))
        windows.addItem(withTitle: L("창 닫기"), action: #selector(NSWindow.performClose(_:)), keyEquivalent: "w")
        windowItem.submenu = windows
        main.addItem(windowItem)
        NSApp.mainMenu = main
    }

    func registerShortcut() {
        var type = EventTypeSpec(eventClass: OSType(kEventClassKeyboard), eventKind: UInt32(kEventHotKeyPressed))
        let pointer = Unmanaged.passUnretained(self).toOpaque()
        InstallEventHandler(GetApplicationEventTarget(), { _, _, userData in
            guard let userData = userData else { return OSStatus(eventNotHandledErr) }
            let app = Unmanaged<AppDelegate>.fromOpaque(userData).takeUnretainedValue()
            app.togglePanel()
            return noErr
        }, 1, &type, pointer, &hotKeyHandler)
        if !bindShortcut(code: shortcutCode, modifiers: shortcutModifiers) {
            hint.stringValue = L("단축키를 사용할 수 없습니다. 설정에서 다른 조합을 지정하세요.")
        }
    }

    func bindShortcut(code: UInt32, modifiers: UInt32) -> Bool {
        let id = EventHotKeyID(signature: 0x57454654, id: 1)
        return RegisterEventHotKey(code, modifiers, id, GetApplicationEventTarget(), 0, &hotKey) == noErr
    }

    func carbonModifiers(_ flags: NSEvent.ModifierFlags) -> UInt32 {
        var value: UInt32 = 0
        if flags.contains(.control) { value |= UInt32(controlKey) }
        if flags.contains(.option) { value |= UInt32(optionKey) }
        if flags.contains(.shift) { value |= UInt32(shiftKey) }
        if flags.contains(.command) { value |= UInt32(cmdKey) }
        return value
    }

    @objc func recordShortcut(_ sender: NSButton) {
        shortcutButton = sender
        if sender.window === settingsWindow { shortcutHelp = generalShortcutHelp }
        if let hotKey = hotKey { UnregisterEventHotKey(hotKey); self.hotKey = nil }
        recordingShortcut = true
        shortcutButton?.title = L("단축키를 누르세요…")
        shortcutHelp?.stringValue = L("⌘, ⌥, ⌃ 중 하나를 포함해 누르세요. Esc로 취소합니다.")
        if let recorder = sender as? ShortcutRecorderButton { recorder.appDelegate = self }
        guard let window = sender.window, window.makeFirstResponder(sender) else {
            recordingShortcut = false
            _ = bindShortcut(code: shortcutCode, modifiers: shortcutModifiers)
            shortcutButton?.title = shortcutName
            shortcutHelp?.stringValue = L("단축키 입력창에 포커스를 둘 수 없습니다. 다시 눌러 시도하세요.")
            return
        }
    }

    func captureShortcut(_ event: NSEvent) {
        if event.keyCode == 53 {
            cancelShortcutRecording()
            shortcutHelp?.stringValue = L("어느 앱에서든 검색창을 앞으로 띄웁니다.")
            return
        }
        let flags = event.modifierFlags.intersection(.deviceIndependentFlagsMask)
        guard !flags.intersection([.command, .option, .control]).isEmpty else {
            shortcutHelp?.stringValue = L("⌘, ⌥, ⌃ 중 하나와 다른 키를 함께 누르세요.")
            return
        }
        var modifiers: UInt32 = 0
        var name = ""
        if flags.contains(.control) { modifiers |= UInt32(controlKey); name += "⌃" }
        if flags.contains(.option) { modifiers |= UInt32(optionKey); name += "⌥" }
        if flags.contains(.shift) { modifiers |= UInt32(shiftKey); name += "⇧" }
        if flags.contains(.command) { modifiers |= UInt32(cmdKey); name += "⌘" }
        let special: [UInt16: String] = [
            36: "Return", 48: "Tab", 49: "Space", 51: "Delete", 53: "Esc", 76: "Keypad Enter",
            115: "Home", 116: "Page Up", 117: "Forward Delete", 119: "End", 121: "Page Down",
            122: "F1", 120: "F2", 99: "F3", 118: "F4", 96: "F5", 97: "F6", 98: "F7", 100: "F8",
            101: "F9", 109: "F10", 103: "F11", 111: "F12",
            123: "←", 124: "→", 125: "↓", 126: "↑"
        ]
        guard let key = special[event.keyCode] ?? event.charactersIgnoringModifiers?.uppercased(), !key.isEmpty else { return }
        name += key
        let oldCode = shortcutCode, oldModifiers = shortcutModifiers
        if let hotKey = hotKey { UnregisterEventHotKey(hotKey); self.hotKey = nil }
        if bindShortcut(code: UInt32(event.keyCode), modifiers: modifiers) {
            UserDefaults.standard.set(Int(event.keyCode), forKey: "shortcutCode")
            UserDefaults.standard.set(Int(modifiers), forKey: "shortcutModifiers")
            UserDefaults.standard.set(name, forKey: "shortcutName")
            shortcutHelp?.stringValue = LF("저장되었습니다. 어느 앱에서든 {0}로 검색창을 열 수 있습니다.", String(describing: name))
            statusItem.menu?.items.first?.title = LF("Weft 검색…   {0}", String(describing: name))
        } else {
            _ = bindShortcut(code: oldCode, modifiers: oldModifiers)
            shortcutHelp?.stringValue = L("이미 사용 중인 단축키입니다. 다른 조합을 지정하세요.")
        }
        recordingShortcut = false
        shortcutButton?.title = shortcutName
        shortcutButton?.window?.makeFirstResponder(nil)
    }

    func cancelShortcutRecording() {
        guard recordingShortcut else { return }
        recordingShortcut = false
        _ = bindShortcut(code: shortcutCode, modifiers: shortcutModifiers)
        shortcutButton?.title = shortcutName
        shortcutButton?.window?.makeFirstResponder(nil)
    }

    func applyAppearance() {
        let theme = UserDefaults.standard.string(forKey: "appearance") ?? "system"
        NSApp.appearance = theme == "light" ? NSAppearance(named: .aqua)
            : theme == "dark" ? NSAppearance(named: .darkAqua) : nil
    }

    @objc func appearanceChanged(_ sender: NSPopUpButton) {
        UserDefaults.standard.set(["system", "light", "dark"][sender.indexOfSelectedItem], forKey: "appearance")
        applyAppearance()
    }

    @objc func launchPreferenceChanged(_ sender: NSButton) {
        UserDefaults.standard.set(sender.state == .on, forKey: "showOnLaunch")
    }

    func makePanel() {
        panel = SearchPanel(contentRect: NSRect(x: 0, y: 0, width: 680, height: 84),
            styleMask: [.borderless, .nonactivatingPanel], backing: .buffered, defer: false)
        panel.escapeHandler = { [weak self] in self?.dismissSearchPanelForEscape() }
        panelMotion = WindowMotion(window: panel)
        panel.level = .floating
        panel.isFloatingPanel = true
        panel.becomesKeyOnlyIfNeeded = false
        panel.hidesOnDeactivate = false
        panel.isMovableByWindowBackground = false
        panel.collectionBehavior = [.canJoinAllSpaces, .fullScreenAuxiliary]
        panel.isReleasedWhenClosed = false
        panel.delegate = self
        panel.isOpaque = false
        panel.backgroundColor = .clear
        panel.hasShadow = true
        panelBackground.material = .popover
        panelBackground.blendingMode = .behindWindow
        panelBackground.state = .active
        panelBackground.wantsLayer = true
        panelBackground.layer?.cornerRadius = 18
        panelBackground.layer?.masksToBounds = true
        panel.contentView = panelBackground
        search.placeholderString = L("파일·폴더 검색")
        search.font = .systemFont(ofSize: 23)
        search.isBezeled = false
        search.drawsBackground = false
        search.focusRingType = .none
        (search.cell as? NSSearchFieldCell)?.searchButtonCell = nil
        (search.cell as? NSSearchFieldCell)?.cancelButtonCell = nil
        search.delegate = self
        search.sendsSearchStringImmediately = true
        searchIconView.contentTintColor = WeftTheme.accent
        recordingDot.wantsLayer = true
        recordingDot.layer?.backgroundColor = NSColor.systemRed.cgColor
        recordingDot.layer?.cornerRadius = 5
        recordingLabel.font = .systemFont(ofSize: 13, weight: .medium)
        recordingLabel.textColor = .labelColor
        recordingLabel.maximumNumberOfLines = 1
        recordingLabel.lineBreakMode = .byTruncatingTail
        recordingStopButton.title = L("완료")
        recordingStopButton.bezelStyle = .rounded
        recordingStopButton.controlSize = .small
        recordingStopButton.font = .systemFont(ofSize: 12, weight: .medium)
        recordingStopButton.target = self
        recordingStopButton.action = #selector(stopVoiceRecording)
        mode.selectedSegment = 0
        modeBadge.target = self
        modeBadge.action = #selector(cycleMode)
        modeBadge.isBordered = false
        modeBadge.font = .systemFont(ofSize: 12, weight: .medium)
        modeBadge.contentTintColor = WeftTheme.accent
        modeBadge.toolTip = L("Tab으로 파일명 / 의미 검색 전환")
        hint.font = .systemFont(ofSize: 11)
        hint.textColor = .secondaryLabelColor
        table.headerView = nil
        table.rowHeight = 54
        table.backgroundColor = .clear
        table.intercellSpacing = NSSize(width: 0, height: 2)
        table.dataSource = self
        table.delegate = self
        table.target = self
        table.doubleAction = #selector(openClicked)
        table.contextMenu = { [weak self] row in
            guard let self = self, self.hits.indices.contains(row), !["command", "shortcut"].contains(self.hits[row].kind) else { return nil }
            let menu = NSMenu()
            for (title, action) in [(L("Finder에서 보기"), #selector(self.revealSelected)),
                                    (L("파일 열기"), #selector(self.openClicked)),
                                    (L("큰 미리보기"), #selector(self.previewSelected)),
                                    (L("경로 복사"), #selector(self.copySelectedPath))] {
                let item = NSMenuItem(title: title, action: action, keyEquivalent: "")
                item.target = self
                menu.addItem(item)
            }
            return menu
        }
        table.frame = NSRect(x: 0, y: 0, width: 656, height: 0)
        table.autoresizingMask = [.width]
        table.addTableColumn(NSTableColumn(identifier: NSUserInterfaceItemIdentifier("files")))
        table.tableColumns[0].width = 656
        resultScroll.documentView = table
        resultScroll.hasVerticalScroller = true
        resultScroll.drawsBackground = false
        divider.boxType = .separator
        for view in [search, modeBadge, resultScroll, hint, divider, searchIconView,
                     recordingDot, recordingLabel, recordingStopButton] as [NSView] {
            view.translatesAutoresizingMaskIntoConstraints = false
            panelBackground.addSubview(view)
        }
        makeInspector(in: panelBackground)
        resultTrailing = resultScroll.trailingAnchor.constraint(equalTo: panelBackground.trailingAnchor, constant: -12)
        resultHeight = resultScroll.heightAnchor.constraint(equalToConstant: 0)
        NSLayoutConstraint.activate([
            search.topAnchor.constraint(equalTo: panelBackground.topAnchor, constant: 28),
            search.leadingAnchor.constraint(equalTo: panelBackground.leadingAnchor, constant: 56),
            search.trailingAnchor.constraint(equalTo: modeBadge.leadingAnchor, constant: -12),
            search.heightAnchor.constraint(equalToConstant: 28),
            searchIconView.leadingAnchor.constraint(equalTo: panelBackground.leadingAnchor, constant: 24),
            searchIconView.centerYAnchor.constraint(equalTo: search.centerYAnchor),
            searchIconView.widthAnchor.constraint(equalToConstant: 22),
            searchIconView.heightAnchor.constraint(equalToConstant: 22),
            modeBadge.trailingAnchor.constraint(equalTo: panelBackground.trailingAnchor, constant: -20),
            modeBadge.centerYAnchor.constraint(equalTo: search.centerYAnchor),
            modeBadge.widthAnchor.constraint(equalToConstant: 110),
            divider.topAnchor.constraint(equalTo: panelBackground.topAnchor, constant: 72),
            divider.leadingAnchor.constraint(equalTo: panelBackground.leadingAnchor, constant: 20),
            divider.trailingAnchor.constraint(equalTo: panelBackground.trailingAnchor, constant: -20),
            resultScroll.topAnchor.constraint(equalTo: divider.bottomAnchor, constant: 8),
            resultScroll.leadingAnchor.constraint(equalTo: panelBackground.leadingAnchor, constant: 12),
            resultTrailing,
            resultHeight,
            hint.leadingAnchor.constraint(equalTo: panelBackground.leadingAnchor, constant: 24),
            hint.trailingAnchor.constraint(equalTo: panelBackground.trailingAnchor, constant: -20),
            hint.bottomAnchor.constraint(equalTo: panelBackground.bottomAnchor, constant: -12)
        ])
        recordingDot.translatesAutoresizingMaskIntoConstraints = false
        recordingLabel.translatesAutoresizingMaskIntoConstraints = false
        recordingStopButton.translatesAutoresizingMaskIntoConstraints = false
        recordingRowCenterY = recordingDot.centerYAnchor.constraint(equalTo: panelBackground.centerYAnchor)
        NSLayoutConstraint.activate([
            recordingDot.leadingAnchor.constraint(equalTo: panelBackground.leadingAnchor, constant: 16),
            recordingRowCenterY,
            recordingDot.widthAnchor.constraint(equalToConstant: 10), recordingDot.heightAnchor.constraint(equalToConstant: 10),
            recordingLabel.leadingAnchor.constraint(equalTo: recordingDot.trailingAnchor, constant: 10),
            recordingLabel.trailingAnchor.constraint(equalTo: recordingStopButton.leadingAnchor, constant: -8),
            recordingLabel.centerYAnchor.constraint(equalTo: recordingDot.centerYAnchor),
            recordingStopButton.trailingAnchor.constraint(equalTo: panelBackground.trailingAnchor, constant: -12),
            recordingStopButton.centerYAnchor.constraint(equalTo: recordingDot.centerYAnchor),
            recordingStopButton.widthAnchor.constraint(equalToConstant: 56),
            recordingStopButton.heightAnchor.constraint(equalToConstant: 30)
        ])
        setRecordingChrome(false)
        resizePanel()
    }

    @objc func showPanel() {
        if voiceCapture.isRecording { stopVoiceRecording(); return }
        panelMotion?.cancel()
        warmDaemon()
        if !panel.isVisible {
            cancelSearch()
            search.stringValue = ""
            hits = []
            table.reloadData()
            resizePanel()
            modeBadge.title = mode.selectedSegment == 0 ? L("파일명 ⇥") : L("의미 ⇥")
        }
        dismissPreview()
        onboarding?.window?.orderOut(nil)
        cancelShortcutRecording()
        settingsWindow?.orderOut(nil)
        presentingSearch = true
        let frame = (NSScreen.main ?? NSScreen.screens[0]).visibleFrame
        panel.setFrameOrigin(NSPoint(x: frame.midX - panel.frame.width / 2,
            y: frame.maxY - panel.frame.height - frame.height * 0.18))
        NSApp.activate(ignoringOtherApps: true)
        panel.orderFrontRegardless()
        panel.makeKeyAndOrderFront(nil)
        focusSearch(selectAll: true)
        DispatchQueue.main.async { [weak self] in
            guard let self = self, self.panel.isVisible else { return }
            self.panel.makeKey()
            self.focusSearch()
            if NSApp.isActive { self.presentingSearch = false }
        }
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.15) { [weak self] in
            guard let self = self, self.panel.isVisible, self.presentingSearch else { return }
            self.panel.makeKeyAndOrderFront(nil)
            self.focusSearch()
            self.presentingSearch = false
        }
        _ = presentUnavailableIndex(Self.searchIndexAvailability(at: db))
    }

    @objc func togglePanel() {
        if panel.isVisible {
            if voiceCapture.isRecording { stopVoiceRecording() }
            else { hide() }
        } else {
            showPanel()
        }
    }

    func dismissSearchPanelForEscape() {
        // Escape must not silently finish a voice memo and hide the recording UI.
        guard !voiceCapture.isRecording else { return }
        hide()
    }

    func hide() {
        // Cancel any in-flight activation recovery before ordering the panel out.
        // AppKit may deliver applicationDidBecomeActive after Escape; leaving this
        // true lets that callback immediately bring the dismissed panel back.
        presentingSearch = false
        if voiceCapture.isRecording { stopVoiceRecording(); return }
        if waitingForVoiceResult {
            panelMotion?.cancel()
            panel.orderOut(nil)
            return
        }
        panelMotion?.cancel(); cancelSearch(); clearInspector(); panel.orderOut(nil)
    }

    func voiceModelDirectory() -> URL {
        FileManager.default.homeDirectoryForCurrentUser
            .appendingPathComponent("Library/Application Support/Weft/Models/speech", isDirectory: true)
    }

    func setRecordingChrome(_ active: Bool) {
        for view in [search, modeBadge, searchIconView, resultScroll, hint, divider, inspector] { view.isHidden = active }
        recordingDot.isHidden = !active
        recordingLabel.isHidden = !active
        recordingStopButton.isHidden = !active
        panel.isMovableByWindowBackground = active
        panelBackground.material = active ? .hudWindow : .popover
        panelBackground.layer?.cornerRadius = active ? 22 : 18
        panelBackground.layer?.maskedCorners = [.layerMinXMinYCorner, .layerMaxXMinYCorner,
            .layerMinXMaxYCorner, .layerMaxXMaxYCorner]
        recordingRowCenterY.constant = 0
        panel.level = .floating
        if !active {
            recordingLabel.toolTip = nil
            recordingStopButton.title = L("완료")
            recordingStopButton.action = #selector(stopVoiceRecording)
        }
    }

    func movePanelForRecording(_ active: Bool) {
        guard let screen = panel.screen ?? NSScreen.main ?? NSScreen.screens.first else { return }
        let visible = screen.visibleFrame
        let width = min(active ? 244 : 680, max(1, visible.width - 20))
        let height: CGFloat = active ? 64 : 84
        let x = active ? visible.maxX - width - 10 : floor(screen.frame.midX - width / 2)
        let y = active
            ? visible.maxY - height - 12
            : visible.maxY - height - visible.height * 0.18
        let frame = NSRect(x: x, y: y, width: width, height: height)
        panelMotion?.move(to: frame, animated: true)
    }

    @objc func beginVoiceRecording() {
        guard !waitingForVoiceResult, !voiceCapture.isRecording else { return }
        recordingStopButton.title = L("완료")
        recordingStopButton.action = #selector(stopVoiceRecording)
        recordingLabel.toolTip = nil
        if !FileManager.default.isExecutableFile(atPath: Bundle.main.resourceURL!.appendingPathComponent("weft-speech").path) {
            let alert = NSAlert()
            alert.messageText = L("음성 기능을 사용할 수 없습니다")
            alert.informativeText = L("현재 앱 빌드에 음성 처리 도구가 포함되어 있지 않습니다. 앱을 다시 빌드하거나 최신 버전을 설치해주세요.")
            alert.addButton(withTitle: L("확인"))
            alert.runModal()
            return
        }
        presentingSearch = true
        NSApp.activate(ignoringOtherApps: true)
        panel.makeKeyAndOrderFront(nil)
        voiceCapture.start { [weak self] result in
            DispatchQueue.main.async {
                guard let self else { return }
                switch result {
                case .failure(let error):
                    if self.voiceStartedAt != nil { self.finishVoiceFailure(error.localizedDescription) }
                    else {
                        self.presentingSearch = false
                        self.showVoiceError(error.localizedDescription, microphonePermission: self.isMicrophonePermissionError(error))
                    }
                case .success:
                    self.voiceStartedAt = Date()
                    self.voiceTimer?.invalidate()
                    self.voiceTimer = Timer.scheduledTimer(withTimeInterval: 0.25, repeats: true) { [weak self] _ in self?.updateVoiceClock() }
                    self.setRecordingChrome(true)
                    self.movePanelForRecording(true)
                    self.updateVoiceClock()
                    self.presentingSearch = false
                }
            }
        }
    }

    func updateVoiceClock() {
        guard let started = voiceStartedAt else { return }
        let seconds = Int(Date().timeIntervalSince(started))
        recordingLabel.stringValue = String(format: L("녹음 중 · %02d:%02d"), seconds / 60, seconds % 60)
    }

    @objc func stopVoiceRecording() {
        guard let audioURL = voiceCapture.stop() else { return }
        voiceTimer?.invalidate(); voiceTimer = nil
        voiceStartedAt = nil
        waitingForVoiceResult = true
        setRecordingChrome(false)
        panelMotion?.cancel()
        panel.orderOut(nil)
        processVoiceRecording(audioURL)
    }

    func processVoiceRecording(_ audioURL: URL) {
        let models = voiceModelDirectory()
        let asr = models.appendingPathComponent("sherpa-onnx-sense-voice-zh-en-ja-ko-yue-int8-2025-09-09")
        let diarization = models.appendingPathComponent("sherpa-onnx-pyannote-segmentation-3-0")
        let embedding = models.appendingPathComponent("3dspeaker_speech_eres2net_base_sv_zh-cn_3dspeaker_16k.onnx")
        let ready = FileManager.default.fileExists(atPath: asr.appendingPathComponent("model.int8.onnx").path)
            && FileManager.default.fileExists(atPath: diarization.appendingPathComponent("model.onnx").path)
            && FileManager.default.fileExists(atPath: embedding.path)
        if ready { runSpeechEngine(audioURL, asr: asr, diarization: diarization, embedding: embedding); return }
        recordingLabel.stringValue = L("음성 모델 다운로드 중…")
        DispatchQueue.global(qos: .utility).async { [weak self] in
            guard let self else { return }
            let installer = Bundle.main.resourceURL!.appendingPathComponent("setup-speech.sh")
            let process = Process(); process.executableURL = URL(fileURLWithPath: "/bin/bash")
            process.arguments = [installer.path, models.path]
            let diagnostics = Pipe(); process.standardOutput = diagnostics; process.standardError = diagnostics
            do {
                try process.run()
                let output = diagnostics.fileHandleForReading.readDataToEndOfFile()
                process.waitUntilExit()
                guard process.terminationStatus == 0 else {
                    let detail = String(data: output, encoding: .utf8) ?? L("모델 다운로드 실패")
                    DispatchQueue.main.async { self.finishVoiceFailure(LF("음성 모델을 내려받지 못했습니다.\n{0}", String(describing: detail))) }
                    return
                }
                DispatchQueue.main.async { self.runSpeechEngine(audioURL, asr: asr, diarization: diarization, embedding: embedding) }
            } catch { DispatchQueue.main.async { self.finishVoiceFailure(error.localizedDescription) } }
        }
    }

    func runSpeechEngine(_ audioURL: URL, asr: URL, diarization: URL, embedding: URL) {
        let executable = Bundle.main.resourceURL!.appendingPathComponent("weft-speech")
        let process = Process(); process.executableURL = executable
        process.arguments = [audioURL.path, asr.path, diarization.path, embedding.path]
        let output = Pipe(); let errors = Pipe()
        process.standardOutput = output; process.standardError = errors
        DispatchQueue.global(qos: .background).async { [weak self] in
            do {
                try process.run()
                // Drain both pipes concurrently: Sherpa can emit enough model
                // diagnostics to fill stderr while stdout is still open.
                var data = Data()
                var diagnosticData = Data()
                let readers = DispatchGroup()
                readers.enter()
                DispatchQueue.global(qos: .utility).async {
                    data = output.fileHandleForReading.readDataToEndOfFile()
                    readers.leave()
                }
                readers.enter()
                DispatchQueue.global(qos: .utility).async {
                    diagnosticData = errors.fileHandleForReading.readDataToEndOfFile()
                    readers.leave()
                }
                process.waitUntilExit()
                readers.wait()
                guard process.terminationStatus == 0,
                      let json = try JSONSerialization.jsonObject(with: data) as? [String: Any],
                      let text = json["text"] as? String, !text.isEmpty else {
                    let diagnostic = String(data: diagnosticData, encoding: .utf8)?.trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
                    let reason = process.terminationReason == .uncaughtSignal
                        ? LF("음성 엔진이 신호 {0}로 종료됐습니다.", String(describing: process.terminationStatus))
                        : LF("음성 엔진이 종료 코드 {0}로 끝났습니다.", String(describing: process.terminationStatus))
                    throw NSError(domain: "WeftSpeech", code: Int(process.terminationStatus), userInfo: [NSLocalizedDescriptionKey: diagnostic.isEmpty ? reason : diagnostic])
                }
                DispatchQueue.main.async { self?.saveVoiceNote(text: text, language: json["language"] as? String ?? "unknown", audioURL: audioURL) }
            } catch { DispatchQueue.main.async { self?.finishVoiceFailure(error.localizedDescription) } }
        }
    }

    func saveVoiceNote(text: String, language: String, audioURL: URL) {
        let recordings = storageDirectory(for: 1)
        let transcripts = storageDirectory(for: 2)
        let stamp = uiDate(Date())
        let base = LF("음성 메모 {0}", String(describing: ISO8601DateFormatter().string(from: Date()).replacingOccurrences(of: ":", with: "-").prefix(16)))
        let note = transcripts.appendingPathComponent(base + ".md")
        let audio = recordings.appendingPathComponent(base + ".caf")
        do {
            try FileManager.default.createDirectory(at: recordings, withIntermediateDirectories: true)
            try FileManager.default.createDirectory(at: transcripts, withIntermediateDirectories: true)
            try FileManager.default.moveItem(at: audioURL, to: audio)
            try (LF("# {0}\n\n- 언어: {1}\n- 녹음 파일: `{2}`\n\n## 전사\n\n{3}\n", String(describing: stamp), String(describing: language), String(describing: audio.path), String(describing: text))).write(to: note, atomically: true, encoding: .utf8)
            waitingForVoiceResult = false
            setRecordingChrome(false)
            showPanel()
            search.stringValue = text
            controlTextDidChange(Notification(name: NSControl.textDidChangeNotification))
            indexNoteFile(note, completionMessage: L("음성 메모 저장 및 검색 인덱싱 완료"))
        } catch { try? FileManager.default.removeItem(at: audioURL); finishVoiceFailure(error.localizedDescription) }
    }

    func indexNoteFile(_ note: URL, completionMessage: String) {
        let process = Process(); process.executableURL = engine
        process.arguments = ["--db", db.path, "index", note.path, "--file-types", "text"]
        process.standardInput = FileHandle.nullDevice
        process.standardOutput = FileHandle.nullDevice
        let log = appLogHandle(); process.standardError = log ?? FileHandle.nullDevice
        process.terminationHandler = { [weak self] process in
            try? log?.close()
            DispatchQueue.main.async {
                guard let self else { return }
                self.hint.stringValue = process.terminationStatus == 0 ? completionMessage : L("파일은 저장됐지만 인덱싱에 실패했습니다. 설정 → 로그를 확인하세요.")
            }
        }
        do { try process.run(); voiceIndexProcess = process }
        catch { finishVoiceFailure(LF("메모는 저장됐지만 인덱싱 시작에 실패했습니다: {0}", String(describing: error.localizedDescription))) }
    }

    func finishVoiceFailure(_ message: String) {
        let shouldPresentError = panel.isVisible
        waitingForVoiceResult = false
        if let url = voiceCapture.stop() { try? FileManager.default.removeItem(at: url) }
        voiceTimer?.invalidate(); voiceTimer = nil
        setRecordingChrome(false); movePanelForRecording(false)
        writeAppError("voice: " + message)
        if shouldPresentError { showVoiceError(message) }
    }

    func isMicrophonePermissionError(_ error: Error) -> Bool {
        guard let captureError = error as? VoiceCapture.CaptureError else { return false }
        if case .permissionDenied = captureError { return true }
        return false
    }

    func showVoiceError(_ message: String, microphonePermission: Bool = false) {
        let alert = NSAlert(); alert.messageText = L("음성 메모 처리에 실패했습니다.")
        let firstLines = message.components(separatedBy: .newlines).prefix(2).joined(separator: "\n")
        let concise = String(firstLines.prefix(220))
        alert.informativeText = message.count > concise.count ? concise + L("…\n자세한 내용은 검색 오류 로그에서 확인하세요.") : concise
        alert.addButton(withTitle: L("확인"))
        if microphonePermission { alert.addButton(withTitle: L("시스템 설정 열기")) }
        if microphonePermission, alert.runModal() == .alertSecondButtonReturn,
           let settings = URL(string: "x-apple.systempreferences:com.apple.preference.security?Privacy_Microphone") {
            NSWorkspace.shared.open(settings)
        } else if !microphonePermission {
            alert.runModal()
        }
    }
    func focusSearch(selectAll: Bool = false) {
        (search.currentEditor() as? NSTextView)?.insertionPointColor = WeftTheme.accent
        if let editor = search.currentEditor(), panel.firstResponder === editor {
            if selectAll { search.selectText(nil) }
            return
        }
        panel.makeFirstResponder(search)
        search.selectText(nil)
        if !selectAll, let editor = search.currentEditor() as? NSTextView {
            editor.allowsUndo = true
            editor.setSelectedRange(NSRange(location: search.stringValue.utf16.count, length: 0))
        }
    }
    func windowDidBecomeKey(_ notification: Notification) {
        if (notification.object as? NSWindow) === panel { focusSearch() }
    }
    func applicationDidBecomeActive(_ notification: Notification) {
        if panel.isVisible && presentingSearch {
            panel.makeKeyAndOrderFront(nil)
            focusSearch()
            presentingSearch = false
        }
    }
    func resizePanel(animated: Bool = true) {
        let old = panel.frame
        let rows = min(hits.count, 5)
        let showDetails = hits.contains { $0.kind != "command" && $0.kind != "shortcut" }
        resultHeight.constant = rows == 0 ? 0 : showDetails ? 440 : CGFloat(rows) * 56
        resultScroll.isHidden = rows == 0
        divider.isHidden = rows == 0
        hint.isHidden = rows == 0 && search.stringValue.isEmpty
        inspector.isHidden = !showDetails
        resultTrailing.constant = showDetails ? -388 : -12
        let width: CGFloat = showDetails ? 1040 : 680
        let height: CGFloat = rows == 0 ? (search.stringValue.isEmpty ? 84 : 116) : 116 + resultHeight.constant
        let target = NSRect(x: old.midX - width / 2, y: old.maxY - height, width: width, height: height)
        if target != old { panelMotion?.move(to: target, animated: animated) }
        if !showDetails { clearInspector() }
    }

    func makeInspector(in background: NSView) {
        inspector.translatesAutoresizingMaskIntoConstraints = false
        background.addSubview(inspector)
        inspectorIcon.imageScaling = .scaleProportionallyUpOrDown
        inspectorName.font = .systemFont(ofSize: 16, weight: .semibold)
        inspectorName.lineBreakMode = .byTruncatingMiddle
        inspectorSubtitle.font = .systemFont(ofSize: 12)
        inspectorSubtitle.textColor = .secondaryLabelColor
        inspectorSubtitle.lineBreakMode = .byTruncatingMiddle
        inspectorMetadata.orientation = .vertical
        inspectorMetadata.alignment = .leading
        inspectorMetadata.spacing = 7
        inspectorContent.wantsLayer = true
        inspectorContent.layer?.cornerRadius = 10
        inspectorContent.layer?.masksToBounds = true
        inspectorContent.layer?.borderWidth = 1
        inspectorContent.layer?.borderColor = NSColor.separatorColor.cgColor
        let information = NSTextField(labelWithString: L("정보"))
        information.font = .systemFont(ofSize: 12, weight: .semibold)
        let rule = NSBox()
        rule.boxType = .separator
        for view in [inspectorName, inspectorSubtitle, information, rule, inspectorMetadata, inspectorContent] as [NSView] {
            view.translatesAutoresizingMaskIntoConstraints = false
            inspector.addSubview(view)
        }
        NSLayoutConstraint.activate([
            inspector.topAnchor.constraint(equalTo: resultScroll.topAnchor),
            inspector.bottomAnchor.constraint(equalTo: resultScroll.bottomAnchor),
            inspector.trailingAnchor.constraint(equalTo: background.trailingAnchor, constant: -20),
            inspector.widthAnchor.constraint(equalToConstant: 344),
            inspectorContent.topAnchor.constraint(equalTo: inspector.topAnchor, constant: 4),
            inspectorContent.leadingAnchor.constraint(equalTo: inspector.leadingAnchor),
            inspectorContent.trailingAnchor.constraint(equalTo: inspector.trailingAnchor),
            inspectorContent.heightAnchor.constraint(equalToConstant: 210),
            inspectorName.topAnchor.constraint(equalTo: inspectorContent.bottomAnchor, constant: 14),
            inspectorName.leadingAnchor.constraint(equalTo: inspector.leadingAnchor),
            inspectorName.trailingAnchor.constraint(equalTo: inspector.trailingAnchor),
            inspectorSubtitle.topAnchor.constraint(equalTo: inspectorName.bottomAnchor, constant: 5),
            inspectorSubtitle.leadingAnchor.constraint(equalTo: inspector.leadingAnchor),
            inspectorSubtitle.trailingAnchor.constraint(equalTo: inspector.trailingAnchor),
            information.topAnchor.constraint(equalTo: inspectorSubtitle.bottomAnchor, constant: 14),
            information.leadingAnchor.constraint(equalTo: inspector.leadingAnchor),
            rule.topAnchor.constraint(equalTo: information.bottomAnchor, constant: 6),
            rule.leadingAnchor.constraint(equalTo: inspector.leadingAnchor),
            rule.trailingAnchor.constraint(equalTo: inspector.trailingAnchor),
            inspectorMetadata.topAnchor.constraint(equalTo: rule.bottomAnchor, constant: 10),
            inspectorMetadata.leadingAnchor.constraint(equalTo: inspector.leadingAnchor),
            inspectorMetadata.trailingAnchor.constraint(equalTo: inspector.trailingAnchor)
        ])
    }

    func addMetadata(_ label: String, _ value: String, tooltip: String? = nil) {
        let key = NSTextField(labelWithString: label)
        key.font = .systemFont(ofSize: 11)
        key.textColor = .secondaryLabelColor
        let text = NSTextField(labelWithString: value)
        text.font = .systemFont(ofSize: 11)
        text.lineBreakMode = .byTruncatingMiddle
        text.toolTip = tooltip ?? value
        text.setAccessibilityLabel(label + ": " + value)
        let row = NSStackView(views: [key, text])
        row.orientation = .horizontal
        row.spacing = 10
        key.widthAnchor.constraint(equalToConstant: 48).isActive = true
        text.setContentCompressionResistancePriority(.defaultLow, for: .horizontal)
        inspectorMetadata.addArrangedSubview(row)
        row.widthAnchor.constraint(equalTo: inspectorMetadata.widthAnchor).isActive = true
    }

    func clearInspector() {
        inspectedHit = nil
        inspectedStamp = ""
        inspectorWork?.cancel()
        inspectorWork = nil
        inspectorPreview?.close()
        inspectorPreview = nil
        inspectorContent.subviews.forEach { $0.removeFromSuperview() }
        inspectorIcon.image = nil
        inspectorName.stringValue = ""
        inspectorSubtitle.stringValue = ""
        inspectorMetadata.arrangedSubviews.forEach { inspectorMetadata.removeArrangedSubview($0); $0.removeFromSuperview() }
    }

    func tableViewSelectionDidChange(_ notification: Notification) { if !replacingResults { updateInspector() } }

    func updateInspector() {
        guard hits.indices.contains(table.selectedRow), panel.isVisible else { clearInspector(); return }
        let hit = hits[table.selectedRow]
        guard hit.kind != "command" && hit.kind != "shortcut" else { clearInspector(); return }
        let url = URL(fileURLWithPath: hit.path)
        let resources = try? url.resourceValues(forKeys: [.fileSizeKey, .contentModificationDateKey, .creationDateKey, .localizedTypeDescriptionKey, .isDirectoryKey])
        let stamp = "\(resources?.fileSize ?? -1):\(resources?.contentModificationDate?.timeIntervalSince1970 ?? -1)"
        if let old = inspectedHit, old.path == hit.path, old.kind == hit.kind, old.chunk_index == hit.chunk_index,
           old.start_line == hit.start_line, old.end_line == hit.end_line, old.context == hit.context,
           inspectedStamp == stamp { return }
        clearInspector()
        inspectedHit = hit; inspectedStamp = stamp
        inspectorName.stringValue = url.lastPathComponent
        guard let values = resources else {
            inspectorSubtitle.stringValue = L("파일이 이동되었거나 삭제되었습니다.")
            addMetadata(L("위치"), hit.path)
            return
        }
        inspectorName.toolTip = url.lastPathComponent
        let types = ["pdf": L("PDF 문서"), "docx": L("Microsoft Word 문서"), "doc": L("Microsoft Word 문서"),
                     "pptx": L("PowerPoint 프레젠테이션"), "ppt": L("PowerPoint 프레젠테이션"),
                     "xlsx": L("Excel 통합 문서"), "xls": L("Excel 통합 문서"),
                     "png": L("PNG 이미지"), "jpg": L("JPEG 이미지"), "jpeg": L("JPEG 이미지"),
                     "heic": L("HEIC 이미지"), "gif": L("GIF 이미지"), "webp": L("WebP 이미지"),
                     "mp3": L("MP3 오디오"), "m4a": L("M4A 오디오"), "wav": L("WAV 오디오"), "flac": L("FLAC 오디오"),
                     "mp4": L("MPEG-4 동영상"), "mov": L("QuickTime 동영상"), "mkv": L("Matroska 동영상")]
        var description = values.isDirectory == true ? L("폴더") : types[url.pathExtension.lowercased()] ?? values.localizedTypeDescription ?? L("파일")
        if values.isDirectory != true, let size = values.fileSize {
            description += " · " + ByteCountFormatter.string(fromByteCount: Int64(size), countStyle: .file)
        }
        inspectorSubtitle.stringValue = description
        inspectorSubtitle.toolTip = description
        let dates = DateFormatter()
        dates.locale = Locale(identifier: "ko_KR")
        dates.dateFormat = "yyyy. M. d.  a h:mm"
        if let date = values.creationDate { addMetadata(L("생성일"), dates.string(from: date)) }
        if let date = values.contentModificationDate { addMetadata(L("수정일"), dates.string(from: date)) }
        if hit.chunk_index != nil || hit.start_line > 0 { addMetadata(L("본문"), LF("{0}–{1}줄", String(describing: hit.start_line), String(describing: hit.end_line))) }
        let home = FileManager.default.homeDirectoryForCurrentUser.path
        let parent = url.deletingLastPathComponent().path
        let location = parent.hasPrefix(home + "/") ? "~" + parent.dropFirst(home.count) : parent
        addMetadata(L("위치"), location, tooltip: parent)
        // A large file icon is the fallback while a native preview is preparing.
        inspectorIcon.image = NSWorkspace.shared.icon(forFile: hit.path)
        inspectorIcon.frame = inspectorContent.bounds.insetBy(dx: 110, dy: 40)
        inspectorIcon.autoresizingMask = [.minXMargin, .maxXMargin, .minYMargin, .maxYMargin]
        inspectorContent.addSubview(inspectorIcon)
        // Metadata is immediate; defer the renderer while arrows move rapidly.
        let work = DispatchWorkItem { [weak self] in
            guard let self = self, self.panel.isVisible else { return }
            self.inspectorContent.layoutSubtreeIfNeeded()
            if (hit.chunk_index != nil || hit.start_line > 0), let context = hit.context, !context.isEmpty {
                let scroll = NSScrollView(frame: self.inspectorContent.bounds)
                scroll.autoresizingMask = [.width, .height]
                scroll.hasVerticalScroller = true
                let text = NSTextView(frame: scroll.bounds)
                text.isEditable = false
                text.font = .systemFont(ofSize: 12)
                text.string = context
                scroll.documentView = text
                self.inspectorIcon.removeFromSuperview()
                self.inspectorContent.addSubview(scroll)
            } else if values.isDirectory != true, ["pdf", "doc", "docx", "docm", "ppt", "pptx", "pptm", "xls", "xlsx", "xlsm", "odt", "ods", "odp", "pages", "key", "numbers"].contains(url.pathExtension.lowercased()) {
                // Native thumbnails keep the full document page centered in the
                // compact inspector; the large Quick Look window stays interactive.
                let view = CenteredFilePreviewView(frame: self.inspectorContent.bounds, url: url, fallback: NSWorkspace.shared.icon(forFile: hit.path))
                view.autoresizingMask = [.width, .height]
                self.inspectorIcon.removeFromSuperview()
                self.inspectorContent.addSubview(view)
            } else if values.isDirectory != true, let view = QLPreviewView(frame: self.inspectorContent.bounds, style: .normal) {
                view.autoresizingMask = [.width, .height]
                view.autostarts = false
                view.shouldCloseWithWindow = false
                self.inspectorIcon.removeFromSuperview()
                self.inspectorContent.addSubview(view)
                self.inspectorPreview = view
                view.previewItem = url as NSURL
            }
        }
        inspectorWork = work
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.15, execute: work)
    }

    @objc func cycleMode() {
        mode.selectedSegment = 1 - mode.selectedSegment
        modeChanged()
    }
    func parsedQuery() -> (text: String, level: String) {
        var parts = search.stringValue.split(whereSeparator: { $0.isWhitespace }).map(String.init)
        var level = "file"
        while let first = parts.first?.lowercased(), ["@file", "@folder", "@chunk", "@all", "@any", "@avoid"].contains(first) {
            if ["@file", "@folder", "@chunk"].contains(first) { level = String(first.dropFirst()) }
            parts.removeFirst()
        }
        return (parts.joined(separator: " "), level)
    }
    func operatorPreset() -> String? {
        search.stringValue.split(whereSeparator: { $0.isWhitespace }).map { $0.lowercased() }
            .prefix(while: { $0.hasPrefix("@") }).last(where: { ["@all", "@any", "@avoid"].contains($0) })
    }
    func selectScope(_ command: String) {
        var query = parsedQuery()
        if search.stringValue.hasPrefix("@") && !search.stringValue.contains(where: { $0.isWhitespace }) { query.text = "" }
        if command == "web" {
            search.stringValue = "@web "
            controlTextDidChange(Notification(name: NSControl.textDidChangeNotification))
            panel.makeFirstResponder(search)
            if let editor = search.currentEditor() as? NSTextView { editor.setSelectedRange(NSRange(location: search.stringValue.utf16.count, length: 0)) }
            return
        }
        if ["all", "any", "avoid"].contains(command) {
            search.stringValue = (query.level == "file" ? "" : "@\(query.level) ") + "@\(command) " + query.text
            mode.selectedSegment = 1
        } else {
            let operation = operatorPreset().map { "\($0) " } ?? ""
            search.stringValue = "@\(command) " + operation + query.text
        }
        controlTextDidChange(Notification(name: NSControl.textDidChangeNotification))
        panel.makeFirstResponder(search)
        if let editor = search.currentEditor() as? NSTextView { editor.setSelectedRange(NSRange(location: search.stringValue.utf16.count, length: 0)) }
    }

    var appLogURL: URL { db.deletingLastPathComponent().appendingPathComponent("desktop-app.log") }
    func appLogHandle() -> FileHandle? {
        do {
            try FileManager.default.createDirectory(at: appLogURL.deletingLastPathComponent(), withIntermediateDirectories: true)
            if !FileManager.default.fileExists(atPath: appLogURL.path) { FileManager.default.createFile(atPath: appLogURL.path, contents: nil) }
            let handle = try FileHandle(forWritingTo: appLogURL)
            try handle.seekToEnd()
            return handle
        } catch { return nil }
    }
    func writeAppError(_ message: String) {
        guard let handle = appLogHandle() else { return }
        defer { try? handle.close() }
        try? handle.write(contentsOf: Data(("[\(Date())] " + String(message.prefix(4000)) + "\n").utf8))
    }
    @objc func openAppLog() {
        if FileManager.default.fileExists(atPath: appLogURL.path) { NSWorkspace.shared.open(appLogURL) }
    }
    func uiDate(_ date: Date) -> String {
        let formatter = DateFormatter()
        formatter.locale = Locale(identifier: AppLanguage.current.rawValue)
        formatter.dateStyle = .medium
        formatter.timeStyle = .short
        return formatter.string(from: date)
    }
    func refreshLanguage() {
        mode.setLabel(L("파일명"), forSegment: 0)
        mode.setLabel(L("의미 검색"), forSegment: 1)
        search.placeholderString = mode.selectedSegment == 0 ? L("파일·폴더 검색") : L("내용으로 검색")
        modeBadge.title = mode.selectedSegment == 0 ? L("파일명 ⇥") : L("의미 ⇥")
        agentShared.title = L("공용 에이전트")
        agentStatus.stringValue = L("설치할 에이전트를 선택하세요.")
        hint.stringValue = L("검색할 파일명이나 폴더를 입력하세요")
        let settingsWereVisible = settingsWindow?.isVisible == true
        settingsWindow?.orderOut(nil)
        settingsWindow = nil
        if let item = statusItem { NSStatusBar.system.removeStatusItem(item) }
        indexStatusContainer = nil
        indexRemainingLabel = nil
        indexRegisteredLabel = nil
        indexProgressBar = nil
        indexingStatusTimer?.invalidate()
        makeMenu()
        updateIndexControls()
        table.reloadData()
        inspectedHit = nil
        updateInspector()
        if settingsWereVisible { showSettings() }
    }
    @objc func languagePreferenceChanged(_ sender: NSPopUpButton) {
        let languages = AppLanguage.allCases
        guard languages.indices.contains(sender.indexOfSelectedItem) else { return }
        AppLanguage.select(languages[sender.indexOfSelectedItem])
    }

    @objc func showOnboarding() {
        hide()
        settingsWindow?.orderOut(nil)
        if onboarding == nil { onboarding = OnboardingController(app: self) }
        onboarding?.showWindow(nil)
        NSApp.activate(ignoringOtherApps: true)
        onboarding?.window?.makeKeyAndOrderFront(nil)
    }

    func warmDaemon(force: Bool = false) {
        guard ProcessInfo.processInfo.environment["WEFT_DESKTOP_NO_WARMUP"] != "1",
            force || (UserDefaults.standard.object(forKey: "warmAtOpen") as? Bool ?? true),
            Self.searchIndexAvailability(at: db) == .ready, daemonProcess?.isRunning != true else { return }
        let process = Process()
        process.executableURL = engine
        process.arguments = ["--db", db.path, "daemon", "--socket", daemonSocket.path, "--idle-timeout", "90", "--no-auto-index"]
        process.standardInput = FileHandle.nullDevice
        process.standardOutput = FileHandle.nullDevice
        let log = appLogHandle()
        process.standardError = log ?? FileHandle.nullDevice
        process.terminationHandler = { process in
            try? log?.close()
        }
        do { try process.run(); daemonProcess = process }
        catch { modeBadge.toolTip = LF("의미 검색 준비 실패: {0}", String(describing: error.localizedDescription)) }
    }
    @objc func warmPreferenceChanged(_ sender: NSButton) {
        UserDefaults.standard.set(sender.state == .on, forKey: "warmAtOpen")
        if sender.state == .off && daemonProcess?.isRunning == true { daemonProcess?.terminate() }
    }

    func semanticArguments(text: String, level: String) -> [String]? {
        let prefix = ["--db", db.path, "--embedding-socket", daemonSocket.path]
        if ["all(", "any(", "avoid(", "file("].contains(where: { text.hasPrefix($0) }) {
            let opens = text.filter { $0 == "(" }.count, closes = text.filter { $0 == ")" }.count
            guard opens == closes else { return nil }
            return prefix + ["q", text + " level \(level) top 30", "--json"]
        }
        let negativeParts = text.components(separatedBy: " - ")
        let positivesText = negativeParts[0]
        let separator = positivesText.contains(" | ") ? " | " : positivesText.contains(" & ") ? " & " : " + "
        let positives = positivesText.components(separatedBy: separator).map { $0.trimmingCharacters(in: .whitespaces) }
        let negatives = negativeParts.dropFirst().map { $0.trimmingCharacters(in: .whitespaces) }
        guard !(positives + negatives).contains(where: { $0.isEmpty }),
            !text.hasSuffix(" +"), !text.hasSuffix(" |"), !text.hasSuffix(" -"), !text.hasSuffix(" &") else { return nil }
        if separator == " | " && positivesText.contains(" + ") {
            func quoted(_ value: String) -> String {
                String(data: try! JSONEncoder().encode(value), encoding: .utf8)!
            }
            var groups: [String] = []
            for group in positives {
                let terms = group.components(separatedBy: " + ").map { $0.trimmingCharacters(in: .whitespaces) }
                guard !terms.contains(where: { $0.isEmpty }) else { return nil }
                groups.append(terms.count == 1 ? quoted(terms[0]) : "all(" + terms.map(quoted).joined(separator: ",") + ")")
            }
            var expression = "any(" + groups.joined(separator: ",") + ")"
            if !negatives.isEmpty {
                let negative = negatives.count == 1 ? quoted(negatives[0]) : "all(" + negatives.map(quoted).joined(separator: ",") + ")"
                expression = "avoid(" + expression + "," + negative + ")"
            }
            return prefix + ["q", expression + " level \(level) top 30", "--json"]
        }
        if level == "file" && positives.count == 1 && negatives.isEmpty && operatorPreset() == nil {
            return prefix + ["search", positives[0], "--mode", "hybrid", "--top", "30", "--json"]
        }
        let combine = operatorPreset() == "@any" || separator == " | " ? "any" : separator == " & " ? "strict" : "all"
        var args = prefix + ["retrieve", positives[0], "--level", level, "--combine", combine,
            "--context-chars", level == "chunk" ? "6000" : "0", "--top", "30", "--json"]
        for term in positives.dropFirst() { args += ["--also", term] }
        for term in negatives { args += ["--avoid", term] }
        return args
    }
    func windowDidResignKey(_ notification: Notification) {
        if (notification.object as? NSWindow) === panel && !presentingSearch && !voiceCapture.isRecording && !waitingForVoiceResult && previewWindow?.isVisible != true {
            // Quick Look and context menus may temporarily take key focus within the app.
            DispatchQueue.main.asyncAfter(deadline: .now() + 0.1) { [weak self] in
                guard let self = self, !NSApp.isActive, !self.presentingSearch,
                      self.previewWindow?.isVisible != true else { return }
                self.hide()
            }
        }
        if (notification.object as? NSWindow) === settingsWindow || (notification.object as? NSWindow) === onboarding?.window { cancelShortcutRecording() }
    }
    func applicationDidResignActive(_ notification: Notification) {
        if !presentingSearch && previewWindow?.isVisible != true && !voiceCapture.isRecording && !waitingForVoiceResult { hide() }
    }
    @objc func quit() { dismissPreview(); NSApp.terminate(nil) }
    @objc func modeChanged() {
        cancelSearch()
        modeBadge.title = mode.selectedSegment == 0 ? L("파일명 ⇥") : L("의미 ⇥")
        search.placeholderString = mode.selectedSegment == 0 ? L("파일·폴더 검색") : L("내용으로 검색")
        hint.stringValue = mode.selectedSegment == 0 ? L("파일명 검색 · 바로 검색") : L("입력하면 의미 검색 결과가 갱신됩니다")
        controlTextDidChange(Notification(name: NSControl.textDidChangeNotification))
        focusSearch()
    }

    func controlTextDidChange(_ notification: Notification) {
        cancelSearch()
        let input = search.stringValue
        let aliases = ["@settings": "@설정", "@note": "@메모", "@record": "@녹음", "@设置": "@설정", "@設定": "@설정", "@笔记": "@메모", "@メモ": "@메모", "@录音": "@녹음", "@録音": "@녹음"]
        let words = input.split(maxSplits: 1, whereSeparator: { $0.isWhitespace })
        let canonical = words.first.flatMap { aliases[$0.lowercased()] }
        let raw = canonical.map { $0 + (words.count > 1 ? " " + words[1] : "") } ?? input
        let value = raw.lowercased().trimmingCharacters(in: .whitespacesAndNewlines)
        if value == "@설정" {
            applyResults([Hit(path: "@설정", kind: "command", score: 0, start_line: 0, end_line: 0)])
            hint.stringValue = L("Enter를 눌러 Weft 설정 열기")
            modeBadge.title = L("설정")
            refreshingResults = false
            return
        }
        if value == "@녹음" {
            applyResults([Hit(path: "@녹음", kind: "command", score: 0, start_line: 0, end_line: 0)])
            hint.stringValue = L("Enter를 눌러 녹음 시작 · 완료 후 전사문을 검색창에 입력합니다")
            modeBadge.title = L("음성 녹음")
            refreshingResults = false
            return
        }
        if value == "@메모" || value.hasPrefix("@메모 ") {
            let title = raw.count > 3 ? String(raw.dropFirst(3)).trimmingCharacters(in: .whitespacesAndNewlines) : ""
            let command = title.isEmpty ? "@메모" : "@메모 " + title
            applyResults([Hit(path: command, kind: "command", score: 0, start_line: 0, end_line: 0)])
            hint.stringValue = L("Enter를 눌러 마크다운 메모 작성 · 저장 후 검색 인덱스에 추가됩니다")
            modeBadge.title = L("새 메모")
            refreshingResults = false
            return
        }
        if value.hasPrefix("@web ") {
            let webQuery = String(raw.trimmingCharacters(in: .whitespacesAndNewlines).dropFirst(5)).trimmingCharacters(in: .whitespacesAndNewlines)
            if webQuery.isEmpty {
                applyResults([])
                hint.stringValue = L("@web 뒤에 검색어를 입력하세요")
            } else {
                applyResults([Hit(path: "@web " + webQuery, kind: "command", score: 0, start_line: 0, end_line: 0)])
                hint.stringValue = L("웹 검색 · ↑↓ 선택 후 Enter · 입력 중에는 브라우저가 열리지 않습니다")
            }
            modeBadge.title = L("웹 검색")
            refreshingResults = false
            return
        }
        if value.hasPrefix("@") && !raw.contains(where: { $0.isWhitespace }) {
            let commands = ["@file", "@folder", "@chunk", "@all", "@any", "@avoid", "@web", "@녹음", "@메모", "@설정"].filter { command in
                command.hasPrefix(value) || aliases.contains { $0.value == command && $0.key.hasPrefix(value) }
            }.map {
                Hit(path: $0, kind: "command", score: 0, start_line: 0, end_line: 0)
            }
            applyResults(commands)
            hint.stringValue = L("↑↓ 범위 선택 · Enter 적용 · Tab 검색 방식 전환")
            return
        }
        if let quickLinks = quickLinkHits(for: raw) {
            applyResults(quickLinks)
            refreshingResults = false
            modeBadge.title = L("바로가기")
            if quickLinks.count == 1,
               let id = UUID(uuidString: quickLinks[0].path),
               let item = userShortcuts.first(where: { $0.id == id }) {
                if item.kind == .app { hint.stringValue = LF("앱 실행 · Enter를 눌러 {0} 열기", String(describing: item.title)) }
                else if quickLinks[0].context != nil && item.target.contains("{query}") { hint.stringValue = L("URL 검색 · Enter를 눌러 실행 · 검색어가 {query}에 들어갑니다") }
                else { hint.stringValue = LF("URL 열기 · Enter를 눌러 {0} 열기", String(describing: item.title)) }
            } else {
                hint.stringValue = L("↑↓ 선택 후 Enter 실행 · 설정 → 바로가기에서 추가")
            }
            return
        }
        modeBadge.title = mode.selectedSegment == 0 ? L("파일명 ⇥") : L("의미 ⇥")
        guard !parsedQuery().text.isEmpty else {
            applyResults([])
            if !presentUnavailableIndex(Self.searchIndexAvailability(at: db)) {
                hint.stringValue = mode.selectedSegment == 0 ? L("검색할 파일명이나 폴더를 입력하세요") : L("입력하면 의미 검색 결과가 갱신됩니다")
            }
            return
        }
        refreshingResults = true
        if operatorPreset() != nil || ["all(", "any(", "avoid("].contains(where: { parsedQuery().text.hasPrefix($0) }) {
            mode.selectedSegment = 1; modeBadge.title = L("의미 ⇥")
        }

        let token = generation
        let work = DispatchWorkItem { [weak self] in
            guard let self = self, self.generation == token else { return }
            self.performSearch()
        }
        debounce = work
        DispatchQueue.main.asyncAfter(deadline: .now() + (mode.selectedSegment == 1 ? 0.35 : 0.18), execute: work)
    }

    func cancelSearch() {
        generation += 1
        refreshingResults = false
        debounce?.cancel()
        debounce = nil
        if searchProcess?.isRunning == true { searchProcess?.terminate() }
        searchProcess = nil
    }

    static func jamoOnly(_ text: String) -> Bool {
        let scalars = text.unicodeScalars.filter { !CharacterSet.whitespacesAndNewlines.contains($0) }
        return !scalars.isEmpty && scalars.allSatisfy {
            (0x1100...0x11FF).contains($0.value) || (0x3130...0x318F).contains($0.value)
                || (0xA960...0xA97F).contains($0.value) || (0xD7B0...0xD7FF).contains($0.value)
        }
    }

    /// Probe metadata only; never create an index, load vectors, or start a model.
    static func searchIndexAvailability(at url: URL) -> SearchIndexAvailability {
        guard FileManager.default.fileExists(atPath: url.path) else { return .missing }
        var database: OpaquePointer?
        let opened = sqlite3_open_v2(url.path, &database, SQLITE_OPEN_READONLY | SQLITE_OPEN_NOMUTEX, nil)
        defer { if let database = database { sqlite3_close(database) } }
        guard opened == SQLITE_OK, let database = database else { return .unavailable }
        sqlite3_busy_timeout(database, 50)
        var statement: OpaquePointer?
        guard sqlite3_prepare_v2(database, "SELECT 1 FROM documents LIMIT 1", -1, &statement, nil) == SQLITE_OK else {
            return .unavailable
        }
        defer { sqlite3_finalize(statement) }
        switch sqlite3_step(statement) {
        case SQLITE_ROW: return .ready
        case SQLITE_DONE: return .empty
        default: return .unavailable
        }
    }

    /// Read only SQLite metadata; safe to poll while indexing without loading the model.
    static func indexedDocumentCount(at url: URL) -> IndexedDocumentCount {
        guard FileManager.default.fileExists(atPath: url.path) else { return .missing }
        var database: OpaquePointer?
        let opened = sqlite3_open_v2(url.path, &database, SQLITE_OPEN_READONLY | SQLITE_OPEN_NOMUTEX, nil)
        defer { if let database = database { sqlite3_close(database) } }
        guard opened == SQLITE_OK, let database = database else { return .unavailable }
        sqlite3_busy_timeout(database, 50)
        var statement: OpaquePointer?
        guard sqlite3_prepare_v2(database, "SELECT COUNT(*) FROM documents", -1, &statement, nil) == SQLITE_OK else {
            return .unavailable
        }
        defer { sqlite3_finalize(statement) }
        guard sqlite3_step(statement) == SQLITE_ROW else { return .unavailable }
        return .available(max(0, Int(sqlite3_column_int64(statement, 0))))
    }

    @discardableResult
    func presentUnavailableIndex(_ availability: SearchIndexAvailability) -> Bool {
        guard availability != .ready else { return false }
        refreshingResults = false
        applyResults([])
        if availability == .unavailable {
            modeBadge.title = L("인덱스 확인 필요")
            hint.stringValue = L("인덱스를 읽을 수 없습니다. 설정 → 인덱싱 또는 검색 오류 로그를 확인하세요.")
        } else if indexProcess?.isRunning == true {
            modeBadge.title = L("인덱싱 중…")
            hint.stringValue = L("초기 인덱싱 중입니다. 첫 파일이 등록되면 검색할 수 있습니다.")
        } else {
            modeBadge.title = L("인덱스 준비 필요")
            hint.stringValue = L("검색할 파일이 아직 없습니다. 메뉴 막대 → 설정 → 인덱싱에서 폴더를 추가하세요.")
        }
        modeBadge.toolTip = hint.stringValue
        return true
    }

    func emptySearchHint(semantic: Bool) -> String {
        semantic ? L("관련도가 충분한 결과가 없습니다. 검색어를 구체화하거나 Tab으로 파일명 검색을 사용하세요.")
            : L("일치하는 파일·폴더가 없습니다. 이름이나 검색 범위를 바꿔보세요.")
    }

    func performSearch(retry: Int = 0) {
        // Some IMEs do not commit promptly, or do not send a final change event.
        // Retain the previous hits but cap composition polling so search cannot
        // stay in the "composing" state forever.
        if (search.currentEditor() as? NSTextView)?.hasMarkedText() == true && !Self.jamoOnly(parsedQuery().text) && retry < 5 {
            modeBadge.title = L("글자 조합 중 · ⇥")
            let token = generation
            let work = DispatchWorkItem { [weak self] in
                guard let self = self, self.generation == token else { return }
                self.performSearch(retry: retry + 1)
            }
            debounce = work
            DispatchQueue.main.asyncAfter(deadline: .now() + 0.10, execute: work)
            return
        }
        cancelSearch()
        let query = parsedQuery()
        guard !query.text.isEmpty else { return }
        guard !presentUnavailableIndex(Self.searchIndexAvailability(at: db)) else { return }
        refreshingResults = true
        let token = generation
        let process = Process()
        process.executableURL = engine
        let initials = mode.selectedSegment == 1 && Self.jamoOnly(query.text)
        let semantic = mode.selectedSegment == 1 && !initials
        if semantic {
            guard let args = semanticArguments(text: query.text, level: query.level) else { refreshingResults = false; modeBadge.title = L("검색식 작성 중 · ⇥"); modeBadge.toolTip = L("연산자 다음 검색어나 닫는 괄호를 입력하세요."); return }
            // Hybrid first tries content identifiers without inference. A
            // lexical miss requests a daemon through the existing retry path.
            if !args.contains("search") || retry > 0 { warmDaemon(force: true) }
            process.arguments = args
        } else {
            process.arguments = ["--db", db.path, "locate", query.text, "--level", initials ? "file" : query.level, "--top", "30", "--json"]
        }
        let output = Pipe()
        process.standardOutput = output
        // Capture diagnostics to disk: stderr must never block a fast typing client.
        let errorURL = FileManager.default.temporaryDirectory.appendingPathComponent("weft-search-\(UUID().uuidString).log")
        FileManager.default.createFile(atPath: errorURL.path, contents: nil)
        let errorHandle = try? FileHandle(forWritingTo: errorURL)
        process.standardError = errorHandle ?? FileHandle.nullDevice
        do { try process.run() }
        catch {
            try? errorHandle?.close()
            try? FileManager.default.removeItem(at: errorURL)
            refreshingResults = false
            modeBadge.title = L("실행 실패 · ⇥")
            modeBadge.toolTip = error.localizedDescription
            writeAppError("launch: " + error.localizedDescription)
            return
        }
        searchProcess = process
        modeBadge.title = L("검색 중…")
        DispatchQueue.global(qos: .userInitiated).async { [weak self] in
            let data = output.fileHandleForReading.readDataToEndOfFile()
            process.waitUntilExit()
            try? errorHandle?.close()
            let errorText = (try? String(contentsOf: errorURL, encoding: .utf8)) ?? ""
            try? FileManager.default.removeItem(at: errorURL)
            let results = (try? JSONDecoder().decode([Hit].self, from: data))
                ?? (try? JSONDecoder().decode(Retrieval.self, from: data).hits)
            DispatchQueue.main.async {
                guard let self = self, self.generation == token else { return }
                self.searchProcess = nil
                if process.terminationStatus != 0 || results == nil {
                    let reason = errorText.isEmpty ? LF("검색 응답 오류 (종료 코드 {0})", String(describing: process.terminationStatus)) : errorText.trimmingCharacters(in: .whitespacesAndNewlines)
                    self.writeAppError(reason)
                    let transient = reason.contains("daemon unavailable") || reason.contains("database is locked") || reason.contains("database is busy")
                    if transient && retry == 0 {
                        self.modeBadge.title = L("다시 연결 중…")
                        self.warmDaemon(force: semantic)
                        let work = DispatchWorkItem { [weak self] in
                            guard let self = self, self.generation == token else { return }
                            self.performSearch(retry: 1)
                        }
                        self.debounce = work
                        DispatchQueue.main.asyncAfter(deadline: .now() + 0.3, execute: work)
                        return
                    }
                    self.refreshingResults = false
                    self.modeBadge.title = semantic && reason.contains("daemon") ? L("준비 실패 · ⇥") : L("검색 실패 · ⇥")
                    self.modeBadge.toolTip = reason + L("\n메뉴 막대 → 검색 오류 로그 열기")
                    self.hint.stringValue = reason
                    return
                }
                self.refreshingResults = false
                self.applyResults(results ?? [])
                self.modeBadge.title = initials ? L("초성 · ⇥") : self.mode.selectedSegment == 0 ? L("파일명 ⇥") : L("의미 ⇥")
                if self.hits.isEmpty { self.modeBadge.title = process.terminationStatus == 0 ? L("0개 · ⇥") : L("오류 · ⇥") }
                if self.hits.isEmpty && self.presentUnavailableIndex(Self.searchIndexAvailability(at: self.db)) { return }
                self.hint.stringValue = process.terminationStatus != 0
                    ? L("검색에 실패했습니다. 모델 파일과 인덱스 설정을 확인하세요.")
                    : self.hits.isEmpty ? self.emptySearchHint(semantic: semantic)
                    : LF("{0}개 · ↑↓ 선택·미리보기 · ⏎ 열기 · ⌥⏎ Finder · ⌘Y 미리보기 · ⇥ 검색 방식", String(describing: self.hits.count))
                self.modeBadge.toolTip = self.hint.stringValue
            }
        }
    }

    func applyResults(_ next: [Hit]) {
        let selected = hits.indices.contains(table.selectedRow) ? hits[table.selectedRow] : nil
        replacingResults = true
        hits = next
        table.reloadData()
        resizePanel()
        if !hits.isEmpty {
            let row = selected.flatMap { old in hits.firstIndex { $0.path == old.path && $0.chunk_index == old.chunk_index } } ?? 0
            table.selectRowIndexes(IndexSet(integer: row), byExtendingSelection: false)
            table.scrollRowToVisible(row)
        }
        replacingResults = false
        updateInspector()
    }

    func numberOfRows(in tableView: NSTableView) -> Int { hits.count }
    func tableView(_ tableView: NSTableView, rowViewForRow row: Int) -> NSTableRowView? { ResultRow() }
    func tableView(_ tableView: NSTableView, viewFor tableColumn: NSTableColumn?, row: Int) -> NSView? {
        let id = NSUserInterfaceItemIdentifier("result")
        let cell = tableView.makeView(withIdentifier: id, owner: self) as? NSTableCellView ?? NSTableCellView()
        if cell.identifier == nil {
            cell.identifier = id
            let icon = NSImageView()
            icon.tag = 3
            icon.contentTintColor = .secondaryLabelColor
            icon.translatesAutoresizingMaskIntoConstraints = false
            cell.addSubview(icon)
            let name = NSTextField(labelWithString: "")
            let path = NSTextField(labelWithString: "")
            name.font = .systemFont(ofSize: 14, weight: .medium)
            path.font = .systemFont(ofSize: 11)
            path.textColor = .secondaryLabelColor
            name.lineBreakMode = .byTruncatingMiddle
            path.lineBreakMode = .byTruncatingMiddle
            name.tag = 1; path.tag = 2
            for label in [name, path] { label.translatesAutoresizingMaskIntoConstraints = false; cell.addSubview(label) }
            NSLayoutConstraint.activate([
                icon.leadingAnchor.constraint(equalTo: cell.leadingAnchor, constant: 12),
                icon.centerYAnchor.constraint(equalTo: cell.centerYAnchor),
                icon.widthAnchor.constraint(equalToConstant: 26),
                icon.heightAnchor.constraint(equalToConstant: 26),
                name.leadingAnchor.constraint(equalTo: icon.trailingAnchor, constant: 12),
                name.trailingAnchor.constraint(equalTo: cell.trailingAnchor, constant: -10),
                name.topAnchor.constraint(equalTo: cell.topAnchor, constant: 8),
                path.leadingAnchor.constraint(equalTo: name.leadingAnchor),
                path.trailingAnchor.constraint(equalTo: name.trailingAnchor),
                path.topAnchor.constraint(equalTo: name.bottomAnchor, constant: 4)
            ])
        }
        let hit = hits[row]
        let name = URL(fileURLWithPath: hit.path).lastPathComponent
        let isWebSearch = hit.kind == "command" && hit.path.lowercased().hasPrefix("@web ")
        let quickLink = hit.kind == "shortcut" ? userShortcuts.first { $0.id.uuidString == hit.path } : nil
        let commands = ["@file": L("파일 검색"), "@folder": L("폴더 검색"), "@chunk": L("본문 청크 검색"), "@all": L("여러 주제를 함께"), "@any": L("주제 중 하나라도"), "@avoid": L("피하고 싶은 주제"), "@web": L("웹 검색"), "@녹음": L("음성 메모 녹음"), "@메모": L("새 마크다운 메모"), "@설정": L("Weft 설정 열기")]
        let description = ["@file": L("@file 검색어 · ⌘1"), "@folder": L("@folder 검색어 · ⌘2"), "@chunk": L("@chunk 검색어 · ⌘3 · Tab으로 의미 검색"), "@all": L("@all 데이터베이스 + 재시도"), "@any": L("@any 여행 | 음식"), "@avoid": L("@avoid 여행 - 광고"), "@web": L("@web 검색어 · Enter를 눌러 기본 브라우저에서 검색"), "@녹음": L("Enter를 눌러 녹음 시작"), "@메모": L("Enter를 눌러 마크다운 메모 작성"), "@설정": L("Enter를 눌러 설정 창 열기")]
        (cell.viewWithTag(1) as? NSTextField)?.stringValue = quickLink.map { $0.title + (hit.context.map { " · \($0)" } ?? "") }
            ?? (isWebSearch ? LF("웹 검색: {0}", String(describing: String(hit.path.dropFirst(5))))
            : hit.kind == "command" ? (commands[hit.path] ?? hit.path)
            : hit.chunk_index != nil && hit.start_line > 0 && hit.end_line >= hit.start_line ? LF("{0} · {1}–{2}줄", String(describing: name), String(describing: hit.start_line), String(describing: hit.end_line)) : name)
        (cell.viewWithTag(2) as? NSTextField)?.stringValue = quickLink.map { "\($0.alias) · \($0.kind == .app ? "앱 열기" : "URL 열기")" }
            ?? (isWebSearch ? L("Enter를 눌러 기본 브라우저에서 검색")
            : hit.kind == "command" ? (description[hit.path] ?? "")
            : hit.context?.split(whereSeparator: { $0.isNewline }).first.map(String.init) ?? hit.path)
        if let icon = cell.viewWithTag(3) as? NSImageView {
            icon.contentTintColor = hit.kind == "command" ? .secondaryLabelColor : nil
            if hit.kind == "command" {
                icon.image = NSImage(systemSymbolName: "at", accessibilityDescription: nil)
            } else if let quickLink = quickLink {
                icon.image = quickLink.kind == .app
                    ? NSWorkspace.shared.icon(forFile: quickLink.target)
                    : NSImage(systemSymbolName: "link", accessibilityDescription: nil) ?? NSWorkspace.shared.icon(forFile: hit.path)
            } else {
                icon.image = NSWorkspace.shared.icon(forFile: hit.path)
            }
        }
        return cell
    }
    func moveSelection(_ delta: Int) {
        guard !hits.isEmpty else { return }
        let row = min(max(table.selectedRow + delta, 0), hits.count - 1)
        table.selectRowIndexes(IndexSet(integer: row), byExtendingSelection: false)
        table.scrollRowToVisible(row)
    }
    @objc func openClicked() { openSelection(reveal: false) }
    @objc func revealSelected() { openSelection(reveal: true) }
    @objc func previewSelected() { showPreview() }
    @objc func copySelectedPath() {
        guard hits.indices.contains(table.selectedRow) else { return }
        NSPasteboard.general.clearContents()
        NSPasteboard.general.setString(hits[table.selectedRow].path, forType: .string)
    }
    func openSelection(reveal: Bool) {
        guard hits.indices.contains(table.selectedRow) else { return }
        if hits[table.selectedRow].kind == "shortcut" {
            let hit = hits[table.selectedRow]
            guard let id = UUID(uuidString: hit.path), let quickLink = userShortcuts.first(where: { $0.id == id }) else { return }
            runQuickLink(quickLink, query: hit.context ?? "")
            return
        }
        if hits[table.selectedRow].kind == "command" {
            let command = hits[table.selectedRow].path
            if command == "@녹음" {
                beginVoiceRecording()
            } else if command == "@메모" || command.hasPrefix("@메모 ") {
                openMemoEditor(title: command == "@메모" ? "" : String(command.dropFirst(4)).trimmingCharacters(in: .whitespacesAndNewlines))
            } else if command == "@설정" {
                showSettings()
            } else if command.lowercased().hasPrefix("@web ") {
                openWebSearch(String(command.dropFirst(5)))
            } else {
                selectScope(String(command.dropFirst()))
            }
            return
        }
        let url = URL(fileURLWithPath: hits[table.selectedRow].path)
        if reveal { NSWorkspace.shared.activateFileViewerSelecting([url]); hide() }
        else if NSWorkspace.shared.open(url) { hide() }
        else { hint.stringValue = L("파일이 이동되었거나 삭제되었습니다. 인덱싱 상태를 확인하세요.") }
    }

    func openWebSearch(_ query: String) {
        guard var components = URLComponents(string: "https://www.google.com/search") else { return }
        components.queryItems = [URLQueryItem(name: "q", value: query)]
        guard let url = components.url, NSWorkspace.shared.open(url) else {
            hint.stringValue = L("브라우저에서 웹 검색을 열지 못했습니다.")
            return
        }
        hide()
    }

    func showPreview() {
        guard hits.indices.contains(table.selectedRow) else { return }
        let hit = hits[table.selectedRow]
        guard hit.kind != "command" && hit.kind != "shortcut" else { return }
        if previewWindow != nil { closePreview(); return }
        // Keep verified chunk excerpts; file/folder results preview their actual source.
        guard (hit.chunk_index != nil || hit.start_line > 0), let context = hit.context, !context.isEmpty else {
            showFilePreview(hit)
            return
        }
        let window = NSPanel(contentRect: NSRect(x: 0, y: 0, width: 640, height: 320),
            styleMask: [.titled, .closable, .nonactivatingPanel], backing: .buffered, defer: false)
        window.title = LF("{0} · {1}–{2}줄", String(describing: URL(fileURLWithPath: hit.path).lastPathComponent), String(describing: hit.start_line), String(describing: hit.end_line))
        window.level = .floating
        window.isReleasedWhenClosed = false
        window.delegate = self
        let scroll = NSScrollView(frame: window.contentView!.bounds)
        scroll.autoresizingMask = [.width, .height]
        scroll.hasVerticalScroller = true
        let text = NSTextView(frame: scroll.bounds)
        text.isEditable = false
        text.isSelectable = true
        text.font = .systemFont(ofSize: 14)
        text.textContainerInset = NSSize(width: 18, height: 18)
        text.string = context
        scroll.documentView = text
        window.contentView!.addSubview(scroll)
        previewWindow = window
        window.center()
        window.makeKeyAndOrderFront(nil)
    }

    func showFilePreview(_ hit: Hit) {
        let url = URL(fileURLWithPath: hit.path)
        guard FileManager.default.fileExists(atPath: url.path) else {
            hint.stringValue = L("파일이 이동되었거나 삭제되어 미리볼 수 없습니다.")
            return
        }
        let window = NSPanel(contentRect: NSRect(x: 0, y: 0, width: 760, height: 540),
            styleMask: [.titled, .closable, .resizable, .nonactivatingPanel], backing: .buffered, defer: false)
        window.title = url.lastPathComponent
        window.level = .floating
        window.isReleasedWhenClosed = false
        window.delegate = self
        guard let view = QLPreviewView(frame: window.contentView!.bounds, style: .normal) else { return }
        view.autoresizingMask = [.width, .height]
        view.autostarts = false
        view.shouldCloseWithWindow = false
        window.contentView!.addSubview(view)
        quickLookView = view
        previewWindow = window
        view.previewItem = url as NSURL
        window.center()
        window.makeKeyAndOrderFront(nil)
    }

    func dismissPreview() {
        // Release Quick Look's remote renderer and stop playback on every exit path.
        quickLookView?.close()
        quickLookView = nil
        let window = previewWindow
        previewWindow = nil
        window?.orderOut(nil)
    }

    func closePreview() {
        dismissPreview()
        panel.makeKeyAndOrderFront(nil)
        focusSearch()
    }

    func settingsPage(title: String, subtitle: String, views: [NSView]) -> NSViewController {
        let controller = NSViewController()
        controller.view = NSView(frame: NSRect(x: 0, y: 0, width: 680, height: 650))
        let scroll = NSScrollView(); scroll.translatesAutoresizingMaskIntoConstraints = false
        scroll.hasVerticalScroller = true; scroll.drawsBackground = false
        controller.view.addSubview(scroll)
        let document = SettingsDocumentView(); document.translatesAutoresizingMaskIntoConstraints = false; scroll.documentView = document
        let heading = NSTextField(labelWithString: title); heading.font = .systemFont(ofSize: 22, weight: .semibold)
        let description = NSTextField(wrappingLabelWithString: subtitle); description.font = .systemFont(ofSize: 12); description.textColor = .secondaryLabelColor
        let stack = NSStackView(views: [heading, description] + views)
        stack.orientation = .vertical; stack.alignment = .leading; stack.spacing = 14
        stack.translatesAutoresizingMaskIntoConstraints = false; document.addSubview(stack)
        NSLayoutConstraint.activate([
            scroll.leadingAnchor.constraint(equalTo: controller.view.leadingAnchor), scroll.trailingAnchor.constraint(equalTo: controller.view.trailingAnchor),
            scroll.topAnchor.constraint(equalTo: controller.view.topAnchor), scroll.bottomAnchor.constraint(equalTo: controller.view.bottomAnchor),
            document.widthAnchor.constraint(equalTo: scroll.contentView.widthAnchor),
            stack.leadingAnchor.constraint(equalTo: document.leadingAnchor, constant: 28), stack.trailingAnchor.constraint(equalTo: document.trailingAnchor, constant: -28),
            stack.topAnchor.constraint(equalTo: document.topAnchor, constant: 24), stack.bottomAnchor.constraint(equalTo: document.bottomAnchor, constant: -28)
        ])
        for view in [description] + views {
            if view is NSStackView || view is NSTextField { view.widthAnchor.constraint(equalTo: stack.widthAnchor).isActive = true }
            if let button = view as? NSButton { button.bezelStyle = .rounded; button.font = .systemFont(ofSize: 12) }
        }
        return controller
    }

    func settingsRow(_ label: String, control: NSView) -> NSView {
        let text = NSTextField(labelWithString: label)
        text.font = .systemFont(ofSize: 13, weight: .medium)
        let row = NSStackView(views: [text, control])
        row.orientation = .horizontal
        row.alignment = .centerY
        row.spacing = 16
        text.widthAnchor.constraint(equalToConstant: 120).isActive = true
        if let button = control as? NSButton { button.bezelStyle = .rounded }
        control.widthAnchor.constraint(equalToConstant: 460).isActive = true
        return row
    }

    @objc func showSettings() {
        dismissPreview()
        hide()
        if let window = settingsWindow { NSApp.activate(ignoringOtherApps: true); window.makeKeyAndOrderFront(nil); return }
        let window = NSWindow(contentRect: NSRect(x: 0, y: 0, width: 680, height: 700),
            styleMask: [.titled, .closable], backing: .buffered, defer: false)
        window.title = L("Weft 설정")
        window.isReleasedWhenClosed = false
        window.delegate = self
        let tabs = NSTabViewController()
        tabs.tabStyle = .toolbar
        let record = ShortcutRecorderButton(title: shortcutName, target: self, action: #selector(recordShortcut(_:)))
        record.appDelegate = self
        record.bezelStyle = .rounded
        record.heightAnchor.constraint(equalToConstant: 28).isActive = true
        record.toolTip = L("클릭한 뒤 사용할 단축키를 누르세요")
        let shortcutDescription = NSTextField(wrappingLabelWithString: L("어느 앱에서든 검색창을 앞으로 띄웁니다. 버튼을 눌러 변경하세요."))
        shortcutDescription.font = .systemFont(ofSize: 12)
        shortcutDescription.textColor = .secondaryLabelColor
        shortcutButton = record; shortcutHelp = shortcutDescription; generalShortcutHelp = shortcutDescription
        let appearance = NSPopUpButton()
        appearance.addItems(withTitles: [L("시스템 설정"), L("밝게"), L("어둡게")])
        appearance.selectItem(at: ["system", "light", "dark"].firstIndex(of: UserDefaults.standard.string(forKey: "appearance") ?? "system") ?? 0)
        appearance.target = self; appearance.action = #selector(appearanceChanged(_:))
        let launch = NSButton(checkboxWithTitle: L("앱을 열 때 검색창 표시"), target: self, action: #selector(launchPreferenceChanged(_:)))
        launch.state = (UserDefaults.standard.object(forKey: "showOnLaunch") as? Bool ?? true) ? .on : .off
        let warm = NSButton(checkboxWithTitle: L("검색창을 열 때 의미 검색 미리 준비"), target: self, action: #selector(warmPreferenceChanged(_:)))
        warm.state = (UserDefaults.standard.object(forKey: "warmAtOpen") as? Bool ?? true) ? .on : .off
        let language = NSPopUpButton()
        language.addItems(withTitles: AppLanguage.allCases.map { $0.nativeName })
        language.selectItem(at: AppLanguage.allCases.firstIndex(of: AppLanguage.current) ?? 0)
        language.target = self; language.action = #selector(languagePreferenceChanged(_:))
        let languageHelp = NSTextField(wrappingLabelWithString: L("설정 언어를 선택하세요. 변경 사항이 즉시 적용됩니다."))
        languageHelp.font = .systemFont(ofSize: 12); languageHelp.textColor = .secondaryLabelColor
        let general = settingsPage(title: L("일반"), subtitle: L("빠르게 열고, 필요한 순간에 검색하세요."), views: [
            settingsRow(L("검색창 단축키"), control: record), shortcutDescription,
            settingsRow(L("언어"), control: language), languageHelp,
            settingsRow(L("화면 스타일"), control: appearance), launch, warm
        ])
        let generalTab = NSTabViewItem(viewController: general)
        generalTab.label = L("일반")
        generalTab.image = NSImage(systemSymbolName: "slider.horizontal.3", accessibilityDescription: nil)
        tabs.addTabViewItem(generalTab)

        let indexTab = NSTabViewItem(viewController: indexedFileSettings())
        indexTab.label = L("인덱싱")
        indexTab.image = NSImage(systemSymbolName: "folder", accessibilityDescription: nil)
        tabs.addTabViewItem(indexTab)

        let voiceTab = NSTabViewItem(viewController: memoAndVoiceSettings())
        voiceTab.label = L("음성·메모")
        voiceTab.image = NSImage(systemSymbolName: "waveform", accessibilityDescription: nil)
        tabs.addTabViewItem(voiceTab)

        let updatesTab = NSTabViewItem(viewController: updateSettings())
        updatesTab.label = L("업데이트")
        updatesTab.image = NSImage(systemSymbolName: "arrow.down.circle", accessibilityDescription: nil)
        tabs.addTabViewItem(updatesTab)

        let linksTab = NSTabViewItem(viewController: quickLinksSettings())
        linksTab.label = L("바로가기")
        linksTab.image = NSImage(systemSymbolName: "link", accessibilityDescription: nil)
        tabs.addTabViewItem(linksTab)

        let agentSettings = agentIntegrationSettings()
        let agentTab = NSTabViewItem(viewController: agentSettings)
        agentTab.label = L("에이전트")
        agentTab.image = NSImage(systemSymbolName: "terminal", accessibilityDescription: nil)
        tabs.addTabViewItem(agentTab)

        let help = usageSettings()
        let helpTab = NSTabViewItem(viewController: help); helpTab.label = L("사용법")
        helpTab.image = NSImage(systemSymbolName: "questionmark.circle", accessibilityDescription: nil)
        tabs.addTabViewItem(helpTab)

        let permissionsTab = NSTabViewItem(viewController: permissionSettings())
        permissionsTab.label = L("권한")
        permissionsTab.image = NSImage(systemSymbolName: "lock.shield", accessibilityDescription: nil)
        tabs.addTabViewItem(permissionsTab)

        let version = Bundle.main.object(forInfoDictionaryKey: "CFBundleShortVersionString") as? String ?? "1.0.0"
        let about = settingsPage(title: "Weft", subtitle: L("가까이에 두는 로컬 파일 검색"), views: [
            NSTextField(labelWithString: LF("앱 버전 {0}", String(describing: version))),
            NSTextField(wrappingLabelWithString: L("사용 방법은 ‘사용법’ 탭에서 확인할 수 있습니다.")),
            settingsSection(L("앱 제거")),
            NSTextField(wrappingLabelWithString: L("Weft 앱, 로컬 인덱스, 설치된 CLI를 휴지통으로 이동합니다. 메모·녹음과 에이전트 스킬은 보존됩니다.")),
            uninstallButton()
        ])
        let aboutTab = NSTabViewItem(viewController: about)
        aboutTab.label = L("정보")
        aboutTab.image = NSImage(systemSymbolName: "info.circle", accessibilityDescription: nil)
        tabs.addTabViewItem(aboutTab)
        window.contentViewController = tabs
        window.setContentSize(NSSize(width: 680, height: 650))
        settingsWindow = window
        window.center()
        NSApp.activate(ignoringOtherApps: true)
        window.makeKeyAndOrderFront(nil)
    }

    func windowWillClose(_ notification: Notification) {
        if (notification.object as? NSWindow) === settingsWindow {
            cancelShortcutRecording()
        }
        if (notification.object as? NSWindow) === onboarding?.window { cancelShortcutRecording() }
        if (notification.object as? NSWindow) === previewWindow { closePreview() }
    }

    @objc func chooseFolder() { presentFolderPicker() }

    func presentFolderPicker(completion: (() -> Void)? = nil) {
        guard indexProcess?.isRunning != true else { indexingLabel?.stringValue = L("감시를 중지한 뒤 폴더를 변경하세요."); return }
        guard folderPicker == nil else { return }
        cancelShortcutRecording()
        let picker = NSOpenPanel()
        folderPicker = picker
        picker.title = L("검색할 폴더 선택")
        picker.prompt = L("선택")
        picker.canChooseDirectories = true; picker.canChooseFiles = false
        picker.allowsMultipleSelection = false; picker.canCreateDirectories = false
        picker.resolvesAliases = true
        picker.treatsFilePackagesAsDirectories = false
        if !folder.isEmpty { picker.directoryURL = URL(fileURLWithPath: folder) }
        let finished: (NSApplication.ModalResponse) -> Void = { [weak self] response in
            guard let self = self else { return }
            if response == .OK, let url = picker.url {
                self.folder = url.path
                self.folderLabel?.stringValue = url.path
                self.folderLabel?.toolTip = url.path
            }
            self.folderPicker = nil
            completion?()
        }
        // An independent open panel works reliably for LSUIElement/menu-bar apps.
        // Do not recreate settings while the picker is already opening.
        NSApp.activate(ignoringOtherApps: true)
        picker.begin(completionHandler: finished)
    }

    @objc func openLog() {
        let log = db.deletingLastPathComponent().appendingPathComponent("desktop-index.log")
        if FileManager.default.fileExists(atPath: log.path) { NSWorkspace.shared.open(log) }
        else { showSettings(); indexingLabel?.stringValue = L("아직 인덱싱 로그가 없습니다.") }
    }

    @objc func toggleIndexing() {
        if indexProcess?.isRunning == true || UserDefaults.standard.bool(forKey: "indexingEnabled") { stopIndexing(); return }
        guard !folder.isEmpty else {
            presentFolderPicker { [weak self] in
                guard let self = self, !self.folder.isEmpty else { return }
                self.toggleIndexing()
            }
            return
        }
        guard !indexTypes.isEmpty else { indexingLabel?.stringValue = L("파일 형식을 하나 이상 선택하세요."); return }
        if indexMode != "manual" { UserDefaults.standard.set(true, forKey: "indexingEnabled") }
        if indexMode == "interval" { scheduleIndexing() }
        startIndexing(watch: indexMode == "watch")
    }

}

let app = NSApplication.shared
let delegate = AppDelegate()
app.delegate = delegate
app.run()
