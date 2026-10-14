import Cocoa

/// First-run setup stays separate from the search panel and never scans without opting in.
final class OnboardingController: NSWindowController, URLSessionDownloadDelegate {
    unowned let appDelegate: AppDelegate
    var step = 0
    let body = NSStackView()
    let titleLabel = NSTextField(labelWithString: "")
    let subtitle = NSTextField(wrappingLabelWithString: "")
    let status = NSTextField(wrappingLabelWithString: "")
    let progress = NSProgressIndicator()
    let next = NSButton(title: "다음", target: nil, action: nil)
    let back = NSButton(title: "이전", target: nil, action: nil)
    let folderLabel = NSTextField(wrappingLabelWithString: "")
    let codex = NSButton(checkboxWithTitle: "Codex", target: nil, action: nil)
    let claude = NSButton(checkboxWithTitle: "Claude Code", target: nil, action: nil)
    let shared = NSButton(checkboxWithTitle: "공용 에이전트 스킬 폴더", target: nil, action: nil)
    let indexAfterSetup = NSButton(checkboxWithTitle: "완료 후 선택한 폴더 인덱싱 및 변경 감시 시작", target: nil, action: nil)
    var session: URLSession?
    var download: URLSessionDownloadTask?
    var validating = false
    var modelReady = false
    var errorMessage: String?
    var modelURL: URL {
        let root = ProcessInfo.processInfo.environment["XDG_CACHE_HOME"].map { URL(fileURLWithPath: $0) }
            ?? FileManager.default.homeDirectoryForCurrentUser.appendingPathComponent(".cache")
        return root.appendingPathComponent("myvec/embeddinggemma-2-Q8_0.gguf")
    }

    init(app: AppDelegate) {
        appDelegate = app
        let window = NSWindow(contentRect: NSRect(x: 0, y: 0, width: 620, height: 460),
            styleMask: [.titled, .closable], backing: .buffered, defer: false)
        window.title = "Weft 시작하기"
        window.isReleasedWhenClosed = false
        super.init(window: window)
        window.delegate = app
        titleLabel.font = .systemFont(ofSize: 25, weight: .semibold)
        subtitle.font = .systemFont(ofSize: 13)
        subtitle.textColor = .secondaryLabelColor
        status.font = .systemFont(ofSize: 12)
        status.textColor = .secondaryLabelColor
        body.orientation = .vertical
        body.alignment = .leading
        body.spacing = 16
        next.target = self; next.action = #selector(advance)
        next.keyEquivalent = "\r"
        WeftTheme.primary(next)
        back.target = self; back.action = #selector(previous)
        let later = NSButton(title: "나중에", target: self, action: #selector(deferSetup))
        let actions = NSStackView(views: [later, back, next])
        actions.spacing = 12
        for view in [titleLabel, subtitle, body, status, actions] as [NSView] {
            view.translatesAutoresizingMaskIntoConstraints = false
            window.contentView!.addSubview(view)
        }
        NSLayoutConstraint.activate([
            titleLabel.leadingAnchor.constraint(equalTo: window.contentView!.leadingAnchor, constant: 32),
            titleLabel.topAnchor.constraint(equalTo: window.contentView!.topAnchor, constant: 28),
            subtitle.leadingAnchor.constraint(equalTo: titleLabel.leadingAnchor),
            subtitle.trailingAnchor.constraint(equalTo: window.contentView!.trailingAnchor, constant: -32),
            subtitle.topAnchor.constraint(equalTo: titleLabel.bottomAnchor, constant: 8),
            body.leadingAnchor.constraint(equalTo: titleLabel.leadingAnchor),
            body.trailingAnchor.constraint(equalTo: subtitle.trailingAnchor),
            body.topAnchor.constraint(equalTo: subtitle.bottomAnchor, constant: 26),
            status.leadingAnchor.constraint(equalTo: titleLabel.leadingAnchor),
            status.trailingAnchor.constraint(equalTo: subtitle.trailingAnchor),
            status.bottomAnchor.constraint(equalTo: actions.topAnchor, constant: -20),
            actions.trailingAnchor.constraint(equalTo: subtitle.trailingAnchor),
            actions.bottomAnchor.constraint(equalTo: window.contentView!.bottomAnchor, constant: -24)
        ])
        shared.state = .on
        progress.widthAnchor.constraint(equalToConstant: 540).isActive = true
        render()
        window.center()
        validateCachedModel()
    }
    required init?(coder: NSCoder) { fatalError("init(coder:) has not been implemented") }

    func add(_ view: NSView) { body.addArrangedSubview(view) }
    func render() {
        appDelegate.cancelShortcutRecording()
        body.arrangedSubviews.forEach { body.removeArrangedSubview($0); $0.removeFromSuperview() }
        back.isEnabled = step > 0
        next.title = step == 3 ? "설정 완료" : "다음"
        status.stringValue = errorMessage ?? "\(step + 1) / 4 · 메뉴 막대에서 언제든 다시 설정할 수 있습니다."
        switch step {
        case 0:
            titleLabel.stringValue = "검색 모델 준비"
            subtitle.stringValue = "EmbeddingGemma 2 · 약 300 MB · 다운로드 후에는 Mac에서 로컬로 검색합니다."
            add(NSTextField(wrappingLabelWithString: "문서 내용으로 검색하기 위해 모델이 필요합니다. 이미 설치된 모델은 확인 후 재사용합니다."))
            let button = NSButton(title: modelReady ? "모델 준비 완료" : download != nil ? "다운로드 취소" : "모델 다운로드", target: self, action: #selector(downloadModel))
            button.isEnabled = !modelReady && !validating
            add(button)
            progress.style = .bar
            progress.isIndeterminate = validating
            progress.minValue = 0; progress.maxValue = 100
            add(progress)
            if validating { progress.startAnimation(nil); status.stringValue = "모델 파일 확인 중…" }
            else { progress.stopAnimation(nil) }
            next.isEnabled = modelReady
        case 1:
            titleLabel.stringValue = "어디에서 찾을까요?"
            subtitle.stringValue = "선택한 폴더를 검색 대상으로 사용하고, 이후 파일 변경도 감시합니다."
            folderLabel.stringValue = appDelegate.folder.isEmpty ? "아직 폴더를 선택하지 않았습니다." : appDelegate.folder
            folderLabel.maximumNumberOfLines = 3
            add(folderLabel)
            add(NSButton(title: "폴더 선택…", target: self, action: #selector(chooseFolder)))
            add(NSTextField(wrappingLabelWithString: "인덱싱은 마지막 단계에서 선택해 시작합니다. 지금 폴더를 골라도 파일을 읽거나 인덱싱하지 않습니다."))
            next.isEnabled = !appDelegate.folder.isEmpty
        case 2:
            titleLabel.stringValue = "검색 단축키"
            subtitle.stringValue = "어느 앱에서든 검색창을 바로 여는 키를 지정하세요."
            let shortcut = ShortcutRecorderButton(title: appDelegate.shortcutName, target: appDelegate, action: #selector(AppDelegate.recordShortcut(_:)))
            shortcut.appDelegate = appDelegate
            let help = NSTextField(wrappingLabelWithString: "기본값은 ⌥Space입니다. 버튼을 누른 후 사용할 조합을 입력하세요.")
            appDelegate.shortcutButton = shortcut; appDelegate.shortcutHelp = help
            add(shortcut); add(help)
            add(NSTextField(wrappingLabelWithString: "↑↓ 결과 선택 · Tab 파일명/의미 검색 · @file / @folder / @chunk 범위 선택"))
            next.isEnabled = true
        default:
            titleLabel.stringValue = "에이전트와 함께 사용"
            subtitle.stringValue = "원하면 코딩 에이전트가 Weft로 로컬 파일을 찾도록 스킬을 설치할 수 있습니다."
            let choices = NSStackView(views: [codex, claude, shared])
            choices.spacing = 18
            add(choices)
            add(NSButton(title: "선택한 에이전트에 스킬 설치", target: self, action: #selector(installSkill)))
            add(NSTextField(wrappingLabelWithString: "설치는 이 버튼을 눌렀을 때만 진행됩니다. CLI와 검색 스킬을 설치하며 기존 파일은 백업합니다. 에이전트 설정을 새로 읽거나 재시작해야 스킬이 나타날 수 있습니다."))
            add(indexAfterSetup)
            next.isEnabled = true
        }
    }
    @objc func advance() {
        guard !appDelegate.recordingShortcut else { status.stringValue = "단축키를 입력하거나 Esc로 취소하세요."; return }
        guard next.isEnabled else { return }
        errorMessage = nil
        if step < 3 { step += 1; render(); return }
        if indexAfterSetup.state == .on && !modelReady {
            status.stringValue = "모델 준비 단계에서 검색 모델을 먼저 설치하세요."
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
                self.errorMessage = valid ? nil : "기존 모델 파일을 확인할 수 없습니다. 다시 다운로드하세요."
                if valid { self.progress.doubleValue = 100 }
                self.render()
            }
        }
    }
    @objc func downloadModel() {
        if let task = download { task.cancel(); download = nil; errorMessage = "다운로드를 취소했습니다. 다시 시도할 수 있습니다."; render(); return }
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
            self.progress.doubleValue = totalBytesExpectedToWrite > 0 ? Double(totalBytesWritten) / Double(totalBytesExpectedToWrite) * 100 : 0
            self.status.stringValue = "다운로드 중 · " + ByteCountFormatter.string(fromByteCount: totalBytesWritten, countStyle: .file)
        }
    }
    func urlSession(_ session: URLSession, downloadTask: URLSessionDownloadTask, didFinishDownloadingTo location: URL) {
        do {
            guard let response = downloadTask.response as? HTTPURLResponse, (200...299).contains(response.statusCode) else { throw NSError(domain: "Weft", code: 1, userInfo: [NSLocalizedDescriptionKey: "모델 서버에서 다운로드를 거부했습니다."]) }
            let file = try FileHandle(forReadingFrom: location)
            let magic = try file.read(upToCount: 4)
            try file.close()
            guard magic == Data("GGUF".utf8) else { throw NSError(domain: "Weft", code: 2, userInfo: [NSLocalizedDescriptionKey: "다운로드된 파일이 검색 모델 형식이 아닙니다."]) }
            try FileManager.default.createDirectory(at: modelURL.deletingLastPathComponent(), withIntermediateDirectories: true)
            let staging = modelURL.deletingLastPathComponent().appendingPathComponent(".model-\(UUID().uuidString).gguf")
            try FileManager.default.moveItem(at: location, to: staging)
            // Validate downloaded tensors before replacing an existing cached model.
            let process = Process(); process.executableURL = appDelegate.engine
            process.arguments = ["--model-file", staging.path, "prepare"]
            process.standardOutput = FileHandle.nullDevice; process.standardError = FileHandle.nullDevice
            do { try process.run(); process.waitUntilExit(); guard process.terminationStatus == 0 else { throw NSError(domain: "Weft", code: 3, userInfo: [NSLocalizedDescriptionKey: "검색 모델을 확인할 수 없습니다. 다시 다운로드하세요."]) }
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
            self.download = nil; self.errorMessage = "다운로드 실패: " + error.localizedDescription; self.render()
        }
    }
    @objc func installSkill() { installSkillFiles(in: FileManager.default.homeDirectoryForCurrentUser) }
    func installSkillFiles(in home: URL) {
        do {
            var targets: [URL] = []
            if codex.state == .on { targets.append(home.appendingPathComponent(".codex/skills")) }
            if claude.state == .on { targets.append(home.appendingPathComponent(".claude/skills")) }
            if shared.state == .on { targets.append(home.appendingPathComponent(".agents/skills")) }
            guard !targets.isEmpty else { status.stringValue = "설치할 에이전트를 하나 이상 선택하세요."; return }
            let cli = home.appendingPathComponent(".cargo/bin/weft")
            try FileManager.default.createDirectory(at: cli.deletingLastPathComponent(), withIntermediateDirectories: true)
            let temporary = cli.deletingLastPathComponent().appendingPathComponent(".weft-\(UUID().uuidString)")
            try FileManager.default.copyItem(at: appDelegate.engine, to: temporary)
            if FileManager.default.fileExists(atPath: cli.path) {
                let backup = cli.deletingLastPathComponent().appendingPathComponent("weft.backup-\(UUID().uuidString)")
                try FileManager.default.copyItem(at: cli, to: backup)
                _ = try FileManager.default.replaceItemAt(cli, withItemAt: temporary)
            } else { try FileManager.default.moveItem(at: temporary, to: cli) }
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
            status.stringValue = "스킬 설치 완료 · 선택한 에이전트에서 weft-semantic-search를 사용할 수 있습니다."
        } catch { status.stringValue = "스킬 설치 실패: " + error.localizedDescription }
    }
}
