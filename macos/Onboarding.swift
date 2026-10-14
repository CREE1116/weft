import Cocoa

private final class OnboardingDocumentView: NSView {
    override var isFlipped: Bool { true }
}

/// First-run setup stays separate from the search panel and never scans without opting in.
final class OnboardingController: NSWindowController, URLSessionDownloadDelegate {
    unowned let appDelegate: AppDelegate
    var step = 0
    var pendingLanguage = AppLanguage.current
    let body = NSStackView()
    let breadcrumb = NSTextField(labelWithString: "")
    let titleLabel = NSTextField(labelWithString: "")
    let subtitle = NSTextField(wrappingLabelWithString: "")
    let status = NSTextField(wrappingLabelWithString: "")
    let progress = NSProgressIndicator()
    let next = NSButton(title: AppLanguage.localize("다음"), target: nil, action: nil)
    let back = NSButton(title: AppLanguage.localize("이전"), target: nil, action: nil)
    let later = NSButton(title: AppLanguage.localize("나중에"), target: nil, action: nil)
    let folderLabel = NSTextField(wrappingLabelWithString: "")
    let codex = NSButton(checkboxWithTitle: "Codex", target: nil, action: nil)
    let claude = NSButton(checkboxWithTitle: "Claude Code", target: nil, action: nil)
    let shared = NSButton(checkboxWithTitle: AppLanguage.localize("공용 에이전트 스킬 폴더"), target: nil, action: nil)
    let indexAfterSetup = NSButton(checkboxWithTitle: AppLanguage.localize("완료 후 선택한 폴더 인덱싱 및 변경 감시 시작"), target: nil, action: nil)
    private var languageObserver: NSObjectProtocol?
    var session: URLSession?
    var download: URLSessionDownloadTask?
    var validating = false
    var modelReady = false
    var errorMessage: String?
    // Override in the isolated app smoke test; normal onboarding uses the user's home.
    var cliInstallationHome = FileManager.default.homeDirectoryForCurrentUser
    var modelURL: URL {
        let root = ProcessInfo.processInfo.environment["XDG_CACHE_HOME"].map { URL(fileURLWithPath: $0) }
            ?? FileManager.default.homeDirectoryForCurrentUser.appendingPathComponent(".cache")
        return root.appendingPathComponent("myvec/embeddinggemma-2-Q8_0.gguf")
    }

    init(app: AppDelegate) {
        appDelegate = app
        let window = NSWindow(contentRect: NSRect(x: 0, y: 0, width: 680, height: 520),
            styleMask: [.titled, .closable, .resizable], backing: .buffered, defer: false)
        window.title = AppLanguage.localize("Weft 시작하기")
        window.contentMinSize = NSSize(width: 680, height: 520)
        window.isReleasedWhenClosed = false
        super.init(window: window)
        window.delegate = app
        let content = window.contentView!
        let logo = NSImageView(image: WeftTheme.menuImage)
        logo.contentTintColor = WeftTheme.accent
        logo.imageScaling = .scaleProportionallyUpOrDown
        logo.setAccessibilityLabel("Weft")
        let brand = NSTextField(labelWithString: "Weft")
        brand.font = .systemFont(ofSize: 18, weight: .semibold)
        let header = NSStackView(views: [logo, brand])
        header.orientation = .horizontal; header.alignment = .centerY; header.spacing = 10
        breadcrumb.font = .systemFont(ofSize: 12)
        breadcrumb.textColor = .secondaryLabelColor
        breadcrumb.lineBreakMode = .byTruncatingTail
        titleLabel.font = .systemFont(ofSize: 27, weight: .semibold)
        titleLabel.setContentCompressionResistancePriority(.defaultLow, for: .horizontal)
        subtitle.font = .systemFont(ofSize: 13)
        subtitle.textColor = .secondaryLabelColor
        status.font = .systemFont(ofSize: 12)
        status.textColor = .secondaryLabelColor
        body.orientation = .vertical
        body.alignment = .leading
        body.spacing = 18
        next.target = self; next.action = #selector(advance)
        next.keyEquivalent = "\r"
        WeftTheme.primary(next)
        back.target = self; back.action = #selector(previous)
        later.target = self; later.action = #selector(deferSetup)
        for button in [back, later] { button.bezelStyle = .rounded }
        let spacer = NSView()
        spacer.setContentHuggingPriority(.defaultLow, for: .horizontal)
        let actions = NSStackView(views: [later, spacer, back, next])
        actions.orientation = .horizontal; actions.spacing = 12; actions.alignment = .centerY
        // A scrollable content area keeps longer translations usable at the minimum window size.
        let scroll = NSScrollView()
        scroll.drawsBackground = false
        scroll.hasVerticalScroller = true
        scroll.autohidesScrollers = true
        let document = OnboardingDocumentView()
        scroll.documentView = document
        document.translatesAutoresizingMaskIntoConstraints = false
        body.translatesAutoresizingMaskIntoConstraints = false
        document.addSubview(body)
        for view in [header, breadcrumb, titleLabel, subtitle, scroll, status, actions] as [NSView] {
            view.translatesAutoresizingMaskIntoConstraints = false
            content.addSubview(view)
        }
        NSLayoutConstraint.activate([
            logo.widthAnchor.constraint(equalToConstant: 32),
            logo.heightAnchor.constraint(equalToConstant: 32),
            header.leadingAnchor.constraint(equalTo: content.leadingAnchor, constant: 36),
            header.topAnchor.constraint(equalTo: content.topAnchor, constant: 22),
            breadcrumb.leadingAnchor.constraint(equalTo: header.leadingAnchor),
            breadcrumb.trailingAnchor.constraint(equalTo: content.trailingAnchor, constant: -36),
            breadcrumb.topAnchor.constraint(equalTo: header.bottomAnchor, constant: 14),
            titleLabel.leadingAnchor.constraint(equalTo: header.leadingAnchor),
            titleLabel.trailingAnchor.constraint(equalTo: breadcrumb.trailingAnchor),
            titleLabel.topAnchor.constraint(equalTo: breadcrumb.bottomAnchor, constant: 20),
            subtitle.leadingAnchor.constraint(equalTo: header.leadingAnchor),
            subtitle.trailingAnchor.constraint(equalTo: breadcrumb.trailingAnchor),
            subtitle.topAnchor.constraint(equalTo: titleLabel.bottomAnchor, constant: 10),
            scroll.leadingAnchor.constraint(equalTo: header.leadingAnchor),
            scroll.trailingAnchor.constraint(equalTo: breadcrumb.trailingAnchor),
            scroll.topAnchor.constraint(equalTo: subtitle.bottomAnchor, constant: 24),
            scroll.bottomAnchor.constraint(equalTo: status.topAnchor, constant: -18),
            document.widthAnchor.constraint(equalTo: scroll.contentView.widthAnchor),
            body.leadingAnchor.constraint(equalTo: document.leadingAnchor),
            body.trailingAnchor.constraint(equalTo: document.trailingAnchor),
            body.topAnchor.constraint(equalTo: document.topAnchor),
            body.bottomAnchor.constraint(equalTo: document.bottomAnchor),
            status.leadingAnchor.constraint(equalTo: header.leadingAnchor),
            status.trailingAnchor.constraint(equalTo: breadcrumb.trailingAnchor),
            status.bottomAnchor.constraint(equalTo: actions.topAnchor, constant: -18),
            actions.leadingAnchor.constraint(equalTo: header.leadingAnchor),
            actions.trailingAnchor.constraint(equalTo: breadcrumb.trailingAnchor),
            actions.bottomAnchor.constraint(equalTo: content.bottomAnchor, constant: -24),
            next.widthAnchor.constraint(greaterThanOrEqualToConstant: 100)
        ])
        shared.state = .on
        languageObserver = NotificationCenter.default.addObserver(forName: AppLanguage.languageChangedNotification,
            object: nil, queue: .main) { [weak self] _ in
                guard let self = self else { return }
                self.pendingLanguage = AppLanguage.current
                self.render()
            }
        render()
        window.center()
        validateCachedModel()
    }
    required init?(coder: NSCoder) { fatalError("init(coder:) has not been implemented") }
    deinit { if let languageObserver = languageObserver { NotificationCenter.default.removeObserver(languageObserver) } }

    // Preview a first-page selection without committing preferences before Next.
    private func L(_ key: String) -> String {
        AppLanguage.localize(key, language: step == 0 ? pendingLanguage : AppLanguage.current)
    }
    private func LF(_ key: String, _ arguments: String...) -> String {
        formatLocalizedTemplate(L(key), arguments: arguments)
    }
    func add(_ view: NSView) {
        body.addArrangedSubview(view)
        if view is NSTextField || view is NSStackView || view === progress {
            view.widthAnchor.constraint(equalTo: body.widthAnchor).isActive = true
        }
    }
    private func note(_ text: String) -> NSTextField {
        let label = NSTextField(wrappingLabelWithString: text)
        label.font = .systemFont(ofSize: 13)
        label.textColor = .secondaryLabelColor
        return label
    }
    func render() {
        appDelegate.cancelShortcutRecording()
        body.arrangedSubviews.forEach { body.removeArrangedSubview($0); $0.removeFromSuperview() }
        window?.title = L("Weft 시작하기")
        back.isEnabled = step > 0
        back.isHidden = step == 0
        back.title = L("이전")
        later.title = L("나중에")
        next.title = step == 4 ? L("설정 완료") : L("다음")
        shared.title = L("공용 에이전트 스킬 폴더")
        indexAfterSetup.title = L("완료 후 선택한 폴더 인덱싱 및 변경 감시 시작")
        let steps = [L("언어"), L("모델"), L("폴더"), L("단축키"), L("에이전트")]
        let trail = NSMutableAttributedString()
        for (index, name) in steps.enumerated() {
            if index > 0 { trail.append(NSAttributedString(string: "   ›   ", attributes: [.foregroundColor: NSColor.tertiaryLabelColor])) }
            trail.append(NSAttributedString(string: name, attributes: [
                .font: NSFont.systemFont(ofSize: 12, weight: index == step ? .semibold : .regular),
                .foregroundColor: index == step ? WeftTheme.accent : NSColor.secondaryLabelColor
            ]))
        }
        breadcrumb.attributedStringValue = trail
        status.stringValue = step == 0 ? LF("{0} / 5 · 메뉴 막대에서 언제든 다시 설정할 수 있습니다.", String(step + 1))
            : errorMessage ?? LF("{0} / 5 · 메뉴 막대에서 언제든 다시 설정할 수 있습니다.", String(step + 1))
        switch step {
        case 0:
            titleLabel.stringValue = L("Weft에 오신 것을 환영합니다")
            subtitle.stringValue = L("먼저 사용할 언어를 선택하세요. 설정에서 언제든 바꿀 수 있습니다.")
            let cards = AppLanguage.allCases.enumerated().map { index, language -> NSButton in
                let button = NSButton(title: language.nativeName, target: self, action: #selector(chooseLanguage(_:)))
                button.tag = index
                button.setButtonType(.pushOnPushOff)
                button.bezelStyle = .regularSquare
                button.controlSize = .large
                button.font = .systemFont(ofSize: 17, weight: .medium)
                button.state = pendingLanguage == language ? .on : .off
                button.image = NSImage(systemSymbolName: pendingLanguage == language ? "checkmark.circle.fill" : "circle",
                    accessibilityDescription: nil)
                button.imagePosition = .imageLeading
                button.contentTintColor = pendingLanguage == language ? WeftTheme.accent : .labelColor
                button.setAccessibilityLabel(language.nativeName)
                button.heightAnchor.constraint(equalToConstant: 64).isActive = true
                return button
            }
            let rows = stride(from: 0, to: cards.count, by: 2).map { start -> NSStackView in
                let row = NSStackView(views: Array(cards[start..<min(start + 2, cards.count)]))
                row.orientation = .horizontal; row.distribution = .fillEqually; row.spacing = 14
                return row
            }
            let grid = NSStackView(views: rows)
            grid.orientation = .vertical; grid.alignment = .leading; grid.spacing = 14
            for row in rows { row.widthAnchor.constraint(equalTo: grid.widthAnchor).isActive = true }
            add(grid)
            add(note(L("내 Mac의 파일을 한곳에서 찾으세요. 모델과 폴더를 준비하는 데 몇 단계면 충분합니다.")))
            next.isEnabled = true
        case 1:
            titleLabel.stringValue = L("검색 모델 준비")
            subtitle.stringValue = L("EmbeddingGemma 2 · 약 300 MB · 다운로드 후에는 Mac에서 로컬로 검색합니다.")
            add(note(L("문서 내용으로 검색하기 위해 모델이 필요합니다. 이미 설치된 모델은 확인 후 재사용합니다.")))
            let button = NSButton(title: modelReady ? AppLanguage.localize("모델 준비 완료") : download != nil ? L("다운로드 취소") : L("모델 다운로드"), target: self, action: #selector(downloadModel))
            button.bezelStyle = .rounded
            button.isEnabled = !modelReady && !validating
            add(button)
            progress.style = .bar
            progress.isIndeterminate = validating || (download != nil && progress.doubleValue == 0)
            progress.minValue = 0; progress.maxValue = 100
            add(progress)
            if validating { progress.startAnimation(nil); status.stringValue = L("모델 파일 확인 중…") }
            else if download != nil {
                if progress.isIndeterminate { progress.startAnimation(nil) }
                status.stringValue = L("모델 다운로드 중…")
            } else { progress.stopAnimation(nil) }
            next.isEnabled = modelReady
        case 2:
            titleLabel.stringValue = L("어디에서 찾을까요?")
            subtitle.stringValue = L("선택한 폴더를 검색 대상으로 사용하고, 이후 파일 변경도 감시합니다.")
            folderLabel.stringValue = appDelegate.folder.isEmpty ? L("아직 폴더를 선택하지 않았습니다.") : appDelegate.folder
            folderLabel.maximumNumberOfLines = 3
            folderLabel.font = .systemFont(ofSize: 14, weight: .medium)
            add(folderLabel)
            let choose = NSButton(title: AppLanguage.localize("폴더 선택…"), target: self, action: #selector(chooseFolder))
            choose.bezelStyle = .rounded
            add(choose)
            add(note(L("인덱싱은 마지막 단계에서 선택해 시작합니다. 지금 폴더를 골라도 파일을 읽거나 인덱싱하지 않습니다.")))
            next.isEnabled = !appDelegate.folder.isEmpty
        case 3:
            titleLabel.stringValue = L("검색 단축키")
            subtitle.stringValue = L("어느 앱에서든 검색창을 바로 여는 키를 지정하세요.")
            let shortcut = ShortcutRecorderButton(title: appDelegate.shortcutName, target: appDelegate, action: #selector(AppDelegate.recordShortcut(_:)))
            shortcut.appDelegate = appDelegate
            shortcut.bezelStyle = .rounded
            let help = note(L("기본값은 ⌥Space입니다. 버튼을 누른 후 사용할 조합을 입력하세요."))
            appDelegate.shortcutButton = shortcut; appDelegate.shortcutHelp = help
            add(shortcut); add(help)
            add(note(L("↑↓ 결과 선택 · Tab 파일명/의미 검색 · @file / @folder / @chunk 범위 선택")))
            next.isEnabled = true
        default:
            titleLabel.stringValue = L("에이전트와 함께 사용")
            subtitle.stringValue = L("원하면 코딩 에이전트가 Weft로 로컬 파일을 찾도록 스킬을 설치할 수 있습니다.")
            let choices = NSStackView(views: [codex, claude, shared])
            choices.orientation = .vertical; choices.alignment = .leading; choices.spacing = 10
            add(choices)
            let install = NSButton(title: AppLanguage.localize("선택한 에이전트에 스킬 설치"), target: self, action: #selector(installSkill))
            install.bezelStyle = .rounded
            add(install)
            add(note(L("설정 완료를 누르면 CLI가 ~/.cargo/bin/weft에 자동 설치됩니다. 위 버튼은 선택한 에이전트에 검색 스킬을 설치합니다. 터미널에서 weft를 사용하려면 ~/.cargo/bin이 PATH에 있어야 합니다.")))
            add(indexAfterSetup)
            next.isEnabled = true
        }
    }
    @objc func chooseLanguage(_ sender: NSButton) {
        guard AppLanguage.allCases.indices.contains(sender.tag) else { return }
        pendingLanguage = AppLanguage.allCases[sender.tag]
        render()
    }
    @objc func advance() {
        guard !appDelegate.recordingShortcut else { status.stringValue = L("단축키를 입력하거나 Esc로 취소하세요."); return }
        guard next.isEnabled else { return }
        errorMessage = nil
        if step == 0 { AppLanguage.select(pendingLanguage) }
        if step < 4 { step += 1; render(); return }
        if indexAfterSetup.state == .on && !modelReady {
            status.stringValue = L("모델 준비 단계에서 검색 모델을 먼저 설치하세요.")
            return
        }
        do { try installCLI(in: cliInstallationHome) }
        catch {
            errorMessage = LF("CLI 설치 실패: {0}", error.localizedDescription)
            status.stringValue = errorMessage!
            return
        }
        UserDefaults.standard.set(true, forKey: "onboardingCompleted")
        window?.orderOut(nil)
        if indexAfterSetup.state == .on && appDelegate.indexProcess?.isRunning != true { appDelegate.toggleIndexing() }
        appDelegate.showPanel()
    }
    @objc func previous() { step = max(0, step - 1); errorMessage = nil; render() }
    @objc func deferSetup() {
        appDelegate.cancelShortcutRecording()
        download?.cancel(); download = nil
        window?.orderOut(nil)
        appDelegate.showPanel()
    }
    @objc func chooseFolder() { appDelegate.presentFolderPicker { [weak self] in self?.render() } }

    func validateCachedModel() {
        guard FileManager.default.fileExists(atPath: modelURL.path) else { return }
        validating = true
        render()
        let process = Process()
        process.executableURL = appDelegate.engine
        process.arguments = ["--model-file", modelURL.path, "prepare"]
        process.standardOutput = FileHandle.nullDevice
        process.standardError = FileHandle.nullDevice
        DispatchQueue.global(qos: .utility).async { [weak self] in
            var valid = false
            do { try process.run(); process.waitUntilExit(); valid = process.terminationStatus == 0 } catch { }
            DispatchQueue.main.async {
                guard let self = self else { return }
                self.validating = false; self.modelReady = valid
                self.errorMessage = valid ? nil : self.L("기존 모델 파일을 확인할 수 없습니다. 다시 다운로드하세요.")
                if valid { self.progress.doubleValue = 100 }
                self.render()
            }
        }
    }
    @objc func downloadModel() {
        if let task = download { task.cancel(); download = nil; errorMessage = L("다운로드를 취소했습니다. 다시 시도할 수 있습니다."); render(); return }
        let configuration = URLSessionConfiguration.ephemeral
        configuration.timeoutIntervalForRequest = 60
        configuration.timeoutIntervalForResource = 1800
        session = URLSession(configuration: configuration, delegate: self, delegateQueue: nil)
        let url = URL(string: "https://huggingface.co/ggml-org/embeddinggemma-2-GGUF/resolve/main/embeddinggemma-2-Q8_0.gguf?download=true")!
        errorMessage = nil
        download = session!.downloadTask(with: url)
        progress.doubleValue = 0
        download!.resume(); render()
    }
    func urlSession(_ session: URLSession, downloadTask: URLSessionDownloadTask,
                    didWriteData bytesWritten: Int64, totalBytesWritten: Int64, totalBytesExpectedToWrite: Int64) {
        DispatchQueue.main.async { [weak self] in
            guard let self = self, self.download === downloadTask else { return }
            self.progress.isIndeterminate = totalBytesExpectedToWrite <= 0
            if self.progress.isIndeterminate { self.progress.startAnimation(nil) }
            else { self.progress.stopAnimation(nil) }
            self.progress.doubleValue = totalBytesExpectedToWrite > 0 ? Double(totalBytesWritten) / Double(totalBytesExpectedToWrite) * 100 : 0
            if self.step == 1 {
                self.status.stringValue = self.LF("다운로드 중 · {0}", ByteCountFormatter.string(fromByteCount: totalBytesWritten, countStyle: .file))
            }
        }
    }
    func urlSession(_ session: URLSession, downloadTask: URLSessionDownloadTask, didFinishDownloadingTo location: URL) {
        do {
            guard let response = downloadTask.response as? HTTPURLResponse, (200...299).contains(response.statusCode) else { throw NSError(domain: "Weft", code: 1, userInfo: [NSLocalizedDescriptionKey: L("모델 서버에서 다운로드를 거부했습니다.")]) }
            let file = try FileHandle(forReadingFrom: location)
            let magic = try file.read(upToCount: 4)
            try file.close()
            guard magic == Data("GGUF".utf8) else { throw NSError(domain: "Weft", code: 2, userInfo: [NSLocalizedDescriptionKey: L("다운로드된 파일이 검색 모델 형식이 아닙니다.")]) }
            try FileManager.default.createDirectory(at: modelURL.deletingLastPathComponent(), withIntermediateDirectories: true)
            let staging = modelURL.deletingLastPathComponent().appendingPathComponent(".model-\(UUID().uuidString).gguf")
            try FileManager.default.moveItem(at: location, to: staging)
            // Validate downloaded tensors before replacing an existing cached model.
            let process = Process(); process.executableURL = appDelegate.engine
            process.arguments = ["--model-file", staging.path, "prepare"]
            process.standardOutput = FileHandle.nullDevice; process.standardError = FileHandle.nullDevice
            do { try process.run(); process.waitUntilExit(); guard process.terminationStatus == 0 else { throw NSError(domain: "Weft", code: 3, userInfo: [NSLocalizedDescriptionKey: L("검색 모델을 확인할 수 없습니다. 다시 다운로드하세요.")]) }
                if FileManager.default.fileExists(atPath: modelURL.path) { _ = try FileManager.default.replaceItemAt(modelURL, withItemAt: staging) }
                else { try FileManager.default.moveItem(at: staging, to: modelURL) }
            } catch { try? FileManager.default.removeItem(at: staging); throw error }
            try? FileManager.default.removeItem(atPath: staging.path + ".weft-layout")
            DispatchQueue.main.async { [weak self] in
                guard let self = self, self.download === downloadTask else { return }
                self.download = nil; self.modelReady = true; self.progress.doubleValue = 100; self.errorMessage = nil; self.render()
            }
        } catch {
            DispatchQueue.main.async { [weak self] in
                guard let self = self, self.download === downloadTask else { return }
                self.download = nil; self.errorMessage = error.localizedDescription; self.render()
            }
        }
    }
    func urlSession(_ session: URLSession, task: URLSessionTask, didCompleteWithError error: Error?) {
        session.finishTasksAndInvalidate()
        guard let error = error else { return }
        DispatchQueue.main.async { [weak self] in
            guard let self = self, self.download === task else { return }
            self.download = nil; self.errorMessage = self.LF("다운로드 실패: {0}", error.localizedDescription); self.render()
        }
    }
    @objc func installSkill() { installSkillFiles(in: FileManager.default.homeDirectoryForCurrentUser) }
    @discardableResult
    func installCLI(in home: URL) throws -> URL {
        let manager = FileManager.default
        let cli = home.appendingPathComponent(".cargo/bin/weft")
        try manager.createDirectory(at: cli.deletingLastPathComponent(), withIntermediateDirectories: true)
        // Reopening setup should not create a new backup for an identical binary.
        if manager.fileExists(atPath: cli.path) && manager.contentsEqual(atPath: cli.path, andPath: appDelegate.engine.path) {
            return cli
        }
        let temporary = cli.deletingLastPathComponent().appendingPathComponent(".weft-\(UUID().uuidString)")
        try manager.copyItem(at: appDelegate.engine, to: temporary)
        defer { try? manager.removeItem(at: temporary) }
        if manager.fileExists(atPath: cli.path) {
            let archive = home.appendingPathComponent(".local/share/myvec/cli-backups")
            try manager.createDirectory(at: archive, withIntermediateDirectories: true)
            try manager.copyItem(at: cli, to: archive.appendingPathComponent("weft-\(UUID().uuidString)"))
            // Replace the inode so macOS keeps the copied binary's code signature.
            _ = try manager.replaceItemAt(cli, withItemAt: temporary)
        } else {
            try manager.moveItem(at: temporary, to: cli)
        }
        return cli
    }
    func installSkillFiles(in home: URL) {
        do {
            var targets: [URL] = []
            if codex.state == .on { targets.append(home.appendingPathComponent(".codex/skills")) }
            if claude.state == .on { targets.append(home.appendingPathComponent(".claude/skills")) }
            if shared.state == .on { targets.append(home.appendingPathComponent(".agents/skills")) }
            guard !targets.isEmpty else { status.stringValue = L("설치할 에이전트를 하나 이상 선택하세요."); return }
            try installCLI(in: home)
            let source = Bundle.main.resourceURL!.appendingPathComponent("weft-semantic-search")
            for target in targets {
                try FileManager.default.createDirectory(at: target, withIntermediateDirectories: true)
                let destination = target.appendingPathComponent("weft-semantic-search")
                if FileManager.default.fileExists(atPath: destination.path) {
                    let backup = target.appendingPathComponent("weft-semantic-search.backup-\(UUID().uuidString)")
                    // Keep backups outside the skill-discovery directory to avoid duplicate skills.
                    let archive = home.appendingPathComponent(".local/share/myvec/skill-backups")
                    try FileManager.default.createDirectory(at: archive, withIntermediateDirectories: true)
                    try FileManager.default.moveItem(at: destination, to: archive.appendingPathComponent(backup.lastPathComponent))
                }
                try FileManager.default.copyItem(at: source, to: destination)
            }
            status.stringValue = L("스킬 설치 완료 · 선택한 에이전트에서 weft-semantic-search를 사용할 수 있습니다.")
        } catch { status.stringValue = LF("스킬 설치 실패: {0}", error.localizedDescription) }
    }
}
