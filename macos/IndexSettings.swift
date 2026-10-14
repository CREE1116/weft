import Cocoa
import Darwin

final class SettingsDocumentView: NSView { override var isFlipped: Bool { true } }

extension AppDelegate {
    static let indexTypeChoices: [(String, String)] = [
        ("text", "텍스트·코드 (TXT, MD, 소스 코드)"), ("pdf", "PDF"),
        ("word", "Word (DOCX, DOCM, ODT)"), ("slides", "슬라이드 (PPTX, PPTM, ODP)"),
        ("sheets", "스프레드시트 (XLSX, XLSM, ODS)"), ("ebooks", "EPUB · HWPX"),
        ("image", "이미지"), ("audio", "음성·음악"), ("video", "동영상"), ("other", "기타 형식 (파일명만)")
    ]
    var indexMode: String { UserDefaults.standard.string(forKey: "indexMode") ?? "watch" }
    var indexInterval: TimeInterval { max(900, UserDefaults.standard.double(forKey: "indexInterval") == 0 ? 3600 : UserDefaults.standard.double(forKey: "indexInterval")) }
    var indexTypes: [String] { UserDefaults.standard.stringArray(forKey: "indexTypes") ?? Self.indexTypeChoices.map { $0.0 } }
    var indexDuty: Int { UserDefaults.standard.integer(forKey: "indexDuty") == 0 ? 25 : UserDefaults.standard.integer(forKey: "indexDuty") }
    var mediaPython: URL { FileManager.default.homeDirectoryForCurrentUser.appendingPathComponent(".local/share/weft/media-venv/bin/python") }

    func indexedFileSettings() -> NSViewController {
        indexConfigurationControls = []
        let path = NSTextField(labelWithString: folder.isEmpty ? "폴더를 선택하세요" : folder)
        path.lineBreakMode = .byTruncatingMiddle
        path.toolTip = folder
        path.font = .systemFont(ofSize: 12)
        folderLabel = path
        let choose = NSButton(title: "선택…", target: self, action: #selector(chooseFolder))
        let folderControl = NSStackView(views: [path, choose]); folderControl.spacing = 10
        path.setContentCompressionResistancePriority(.defaultLow, for: .horizontal)
        let mode = NSPopUpButton(); mode.addItems(withTitles: ["파일 변경 시 자동 갱신", "정기적으로 갱신", "수동 갱신"])
        mode.selectItem(at: ["watch", "interval", "manual"].firstIndex(of: indexMode) ?? 0)
        mode.tag = 1; mode.target = self; mode.action = #selector(indexPreferenceChanged(_:))
        let interval = NSPopUpButton(); interval.addItems(withTitles: ["15분", "1시간", "6시간", "하루"])
        interval.selectItem(at: [900.0, 3600, 21600, 86400].firstIndex(of: indexInterval) ?? 1)
        interval.tag = 2; interval.target = self; interval.action = #selector(indexPreferenceChanged(_:))
        let duty = NSPopUpButton(); duty.addItems(withTitles: ["가볍게 (25%)", "보통 (50%)", "빠르게 (100%)"])
        duty.selectItem(at: [25, 50, 100].firstIndex(of: indexDuty) ?? 0)
        duty.tag = 3; duty.target = self; duty.action = #selector(indexPreferenceChanged(_:))
        let launch = NSButton(checkboxWithTitle: "앱을 실행할 때 한 번 갱신", target: self, action: #selector(indexPreferenceChanged(_:)))
        launch.tag = 4; launch.state = UserDefaults.standard.bool(forKey: "indexAtLaunch") ? .on : .off
        let media = NSButton(checkboxWithTitle: "이미지·음성·동영상 내용을 분석", target: self, action: #selector(indexPreferenceChanged(_:)))
        media.tag = 5; media.state = UserDefaults.standard.bool(forKey: "indexMediaContent") ? .on : .off
        let prepare = NSButton(title: "미디어 분석 준비…", target: self, action: #selector(prepareMedia))
        prepare.tag = 6
        let mediaRow = NSStackView(views: [media, prepare]); mediaRow.spacing = 12
        let formats = NSStackView(); formats.orientation = .vertical; formats.alignment = .leading; formats.spacing = 6
        for row in stride(from: 0, to: Self.indexTypeChoices.count, by: 2) {
            var cells: [NSView] = []
            for i in row..<min(row + 2, Self.indexTypeChoices.count) {
                let choice = Self.indexTypeChoices[i]
                let checkbox = NSButton(checkboxWithTitle: choice.1, target: self, action: #selector(indexPreferenceChanged(_:)))
                checkbox.font = .systemFont(ofSize: 11)
                checkbox.tag = 100 + i; checkbox.state = indexTypes.contains(choice.0) ? .on : .off
                checkbox.widthAnchor.constraint(equalToConstant: 280).isActive = true
                cells.append(checkbox); indexConfigurationControls.append(checkbox)
            }
            let pair = NSStackView(views: cells); pair.spacing = 16; formats.addArrangedSubview(pair)
        }
        let status = NSTextField(wrappingLabelWithString: ""); status.font = .systemFont(ofSize: 12); status.textColor = .secondaryLabelColor
        indexingLabel = status
        let toggle = NSButton(title: "자동 갱신 시작", target: self, action: #selector(toggleIndexing)); WeftTheme.primary(toggle); indexButton = toggle
        let refresh = NSButton(title: "지금 갱신", target: self, action: #selector(refreshIndexNow))
        let log = NSButton(title: "로그 보기", target: self, action: #selector(openLog))
        let actions = NSStackView(views: [toggle, refresh, log]); actions.spacing = 10
        let remove = NSButton(title: "인덱스를 휴지통으로 이동…", target: self, action: #selector(deleteIndex))
        remove.hasDestructiveAction = true
        let notice = NSTextField(wrappingLabelWithString: "미디어는 기본적으로 파일명으로 찾습니다. 내용 분석은 별도 Python 환경과 더 큰 모델이 필요합니다. 형식을 변경한 뒤 갱신하면 선택하지 않은 형식은 이 폴더의 인덱스에서 제외됩니다.")
        notice.font = .systemFont(ofSize: 11); notice.textColor = .secondaryLabelColor
        indexConfigurationControls += [choose, mode, interval, duty, launch, media, prepare]
        let page = settingsPage(title: "인덱싱", subtitle: "검색 범위와 갱신 방식을 관리합니다.", views: [
            settingsRow("검색 폴더", control: folderControl), settingsRow("갱신 방식", control: mode),
            settingsRow("갱신 주기", control: interval), settingsRow("작업 속도", control: duty),
            launch, settingsSection("파일 형식"), formats, settingsSection("미디어 내용"), mediaRow, notice,
            settingsSection("실행 상태"), status, actions, settingsSection("인덱스 관리"), remove
        ])
        updateIndexControls()
        return page
    }
    func settingsSection(_ title: String) -> NSView {
        let text = NSTextField(labelWithString: title); text.font = .systemFont(ofSize: 12, weight: .semibold)
        return text
    }
    @objc func indexPreferenceChanged(_ sender: NSControl) {
        let startWatching = (sender as? NSPopUpButton).map { $0.tag == 1 && $0.indexOfSelectedItem == 0 } ?? false
        if let popup = sender as? NSPopUpButton {
            switch popup.tag {
            case 1: UserDefaults.standard.set(["watch", "interval", "manual"][popup.indexOfSelectedItem], forKey: "indexMode")
            case 2: UserDefaults.standard.set([900.0, 3600, 21600, 86400][popup.indexOfSelectedItem], forKey: "indexInterval")
            case 3: UserDefaults.standard.set([25, 50, 100][popup.indexOfSelectedItem], forKey: "indexDuty")
            default: break
            }
        } else if let checkbox = sender as? NSButton {
            if checkbox.tag >= 100 {
                let category = Self.indexTypeChoices[checkbox.tag - 100].0
                var values = Set(indexTypes)
                if checkbox.state == .on { values.insert(category) } else { values.remove(category) }
                UserDefaults.standard.set(values.sorted(), forKey: "indexTypes")
            } else if checkbox.tag == 4 { UserDefaults.standard.set(checkbox.state == .on, forKey: "indexAtLaunch") }
            else if checkbox.tag == 5 {
                if checkbox.state == .on && !FileManager.default.isExecutableFile(atPath: mediaPython.path) {
                    checkbox.state = .off
                    indexingLabel?.stringValue = "먼저 ‘미디어 분석 준비’를 눌러 환경을 설치하세요."
                    return
                }
                UserDefaults.standard.set(checkbox.state == .on, forKey: "indexMediaContent")
            }
        }
        indexTimer?.invalidate(); indexTimer = nil
        UserDefaults.standard.set(false, forKey: "indexingEnabled")
        updateIndexControls()
        if startWatching { enableIndexWatching() }
    }
    func enableIndexWatching() {
        guard indexProcess?.isRunning != true else { return }
        guard !indexTypes.isEmpty else { indexingLabel?.stringValue = "파일 형식을 하나 이상 선택하세요."; return }
        guard !folder.isEmpty else {
            presentFolderPicker { [weak self] in self?.enableIndexWatching() }
            return
        }
        UserDefaults.standard.set(true, forKey: "indexingEnabled")
        startIndexing(watch: true)
    }
    func updateIndexControls() {
        let running = indexProcess?.isRunning == true || mediaSetupProcess?.isRunning == true
        updateMenuBarIndexProgress(running: indexProcess?.isRunning == true)
        for control in indexConfigurationControls { control.isEnabled = !running }
        if let interval = indexConfigurationControls.first(where: { $0.tag == 2 }) { interval.isEnabled = !running && indexMode == "interval" }
        indexButton?.isEnabled = mediaSetupProcess?.isRunning != true
        updateMediaPreparationButton(running: running, ready: FileManager.default.isExecutableFile(atPath: mediaPython.path))
        let enabled = UserDefaults.standard.bool(forKey: "indexingEnabled")
        indexButton?.title = running || enabled ? "중지" : indexMode == "manual" ? "지금 갱신" : "자동 갱신 시작"
        indexToggleMenuItem?.title = running || enabled ? "인덱싱 감시 중지" : indexMode == "manual" ? "지금 인덱싱" : "인덱싱 감시 시작"
        if running {
            if mediaSetupProcess?.isRunning == true {
                indexingLabel?.stringValue = "미디어 분석 환경 준비 중…"
            } else if indexMode == "watch" {
                let scanFinished = indexLogProgressFields()?["scan_complete"] == "true"
                indexingLabel?.stringValue = scanFinished ? "초기 인덱싱 완료 · 파일 변경 감시 중" : "초기 인덱싱 중 · 완료 후 파일 변경 감시"
            } else {
                indexingLabel?.stringValue = "선택한 폴더를 갱신 중"
            }
        }
        else if enabled { indexingLabel?.stringValue = "정기 갱신 대기 중 · 다음 주기부터 검색창이 닫혀 있을 때 실행합니다." }
        else { indexingLabel?.stringValue = "중지됨 · 설정은 저장되며, 시작 또는 지금 갱신을 눌러 적용합니다." }
    }
    func updateMediaPreparationButton(running: Bool, ready: Bool) {
        guard let button = indexConfigurationControls.first(where: { $0.tag == 6 }) as? NSButton else { return }
        button.isEnabled = !running && !ready
        if ready { button.title = "미디어 분석 준비 완료" }
    }
    func updateMenuBarIndexProgress(running: Bool) {
        indexingMenuItem?.isHidden = !running
        guard let button = statusItem?.button else { return }
        if running {
            if indexStatusContainer == nil { installIndexStatusView(on: button) }
            indexingStatusTimer?.invalidate()
            indexingStatusTimer = Timer.scheduledTimer(withTimeInterval: 1.5, repeats: true) { [weak self] _ in
                self?.refreshMenuBarIndexProgress()
            }
            refreshMenuBarIndexProgress()
        } else {
            indexingStatusTimer?.invalidate()
            indexingStatusTimer = nil
            indexStatusContainer?.removeFromSuperview()
            indexStatusContainer = nil
            indexProgressIcon = nil
            indexRemainingLabel = nil
            indexRegisteredLabel = nil
            indexProgressBar = nil
            indexStatusDetailsWidthConstraint = nil
            indexStatusBarWidthConstraint = nil
            statusItem.length = NSStatusItem.squareLength
            button.isBordered = true
            button.title = ""
            button.image = WeftTheme.menuImage
            button.image?.isTemplate = true
            button.toolTip = "Weft · 검색 및 설정"
        }
    }
    func installIndexStatusView(on button: NSStatusBarButton) {
        statusItem.length = 72
        button.image = nil
        button.title = ""
        button.isBordered = false
        button.bezelStyle = .regularSquare

        let icon = NSImageView(image: WeftTheme.menuImage)
        icon.imageScaling = .scaleProportionallyDown
        icon.translatesAutoresizingMaskIntoConstraints = false
        icon.widthAnchor.constraint(equalToConstant: 18).isActive = true
        icon.heightAnchor.constraint(equalToConstant: 18).isActive = true

        let remaining = NSTextField(labelWithString: "남음")
        let registered = NSTextField(labelWithString: "등록 0개")
        for label in [remaining, registered] {
            label.font = .systemFont(ofSize: 8.5, weight: .medium)
            label.textColor = .labelColor
            label.lineBreakMode = .byTruncatingTail
            label.setContentCompressionResistancePriority(.defaultLow, for: .horizontal)
        }
        let bar = IndexProgressBarView()
        bar.translatesAutoresizingMaskIntoConstraints = false
        bar.heightAnchor.constraint(equalToConstant: 3).isActive = true

        let details = NSStackView(views: [remaining, registered, bar])
        details.orientation = .vertical
        details.alignment = .leading
        details.spacing = 0
        details.translatesAutoresizingMaskIntoConstraints = false
        let detailsWidth = details.widthAnchor.constraint(equalToConstant: 46)
        detailsWidth.isActive = true
        details.heightAnchor.constraint(equalToConstant: 22).isActive = true
        let barWidth = bar.widthAnchor.constraint(equalToConstant: 36)
        barWidth.isActive = true

        let container = NSStackView(views: [icon, details])
        container.orientation = .horizontal
        container.alignment = .centerY
        container.spacing = 3
        container.translatesAutoresizingMaskIntoConstraints = false
        button.addSubview(container)
        NSLayoutConstraint.activate([
            container.leadingAnchor.constraint(equalTo: button.leadingAnchor, constant: 2),
            container.trailingAnchor.constraint(equalTo: button.trailingAnchor, constant: -2),
            container.centerYAnchor.constraint(equalTo: button.centerYAnchor),
            container.heightAnchor.constraint(equalToConstant: 22)
        ])
        indexStatusContainer = container
        indexProgressIcon = icon
        indexRemainingLabel = remaining
        indexRegisteredLabel = registered
        indexProgressBar = bar
        indexStatusDetailsWidthConstraint = detailsWidth
        indexStatusBarWidthConstraint = barWidth
    }
    func refreshMenuBarIndexProgress() {
        let fields = indexLogProgressFields()
        let scanned = Int(fields?["scanned"] ?? "0") ?? 0
        let savedTarget = UserDefaults.standard.integer(forKey: "indexProgressTarget")
        if fields?["scan_complete"] == "true", scanned > savedTarget {
            UserDefaults.standard.set(scanned, forKey: "indexProgressTarget")
        }
        let target = savedTarget
        let complete = fields?["scan_complete"] == "true"
        let progress: CGFloat = complete ? 1 : target > 0 ? min(0.96, CGFloat(scanned) / CGFloat(target) * 0.96) : 0
        let added = Int(fields?["indexed"] ?? "") ?? UserDefaults.standard.integer(forKey: "indexLastAddedCount")
        if fields?["scan_complete"] == "true" {
            UserDefaults.standard.set(added, forKey: "indexLastAddedCount")
        }
        let remainingText: String
        let remainingValue: String
        let registeredText: String
        if complete {
            remainingText = "남음"
            remainingValue = "0개"
            registeredText = "등록"
        } else if fields != nil && target > 0 {
            remainingText = "남음"
            remainingValue = "~\(max(0, target - scanned))개"
            registeredText = "등록"
        } else if fields != nil {
            remainingText = "확인"
            remainingValue = "\(scanned)개"
            registeredText = "등록"
        } else if indexMode == "watch" && savedTarget > 0 {
            remainingText = "감시 중"
            remainingValue = ""
            registeredText = "등록"
        } else {
            remainingText = "남음"
            remainingValue = "집계 중"
            registeredText = "등록"
        }
        indexRemainingLabel?.attributedStringValue = statusLine(title: remainingText, value: remainingValue, valueColor: .systemOrange)
        indexRegisteredLabel?.attributedStringValue = statusLine(title: registeredText, value: "\(added)개", valueColor: WeftTheme.accent)
        let contentWidth = ceil(max(
            indexRemainingLabel?.attributedStringValue.size().width ?? 0,
            indexRegisteredLabel?.attributedStringValue.size().width ?? 0,
            36
        ))
        indexStatusDetailsWidthConstraint?.constant = contentWidth
        indexStatusBarWidthConstraint?.constant = contentWidth
        statusItem.length = ceil(18 + 3 + contentWidth + 4)
        indexProgressBar?.progress = progress
        indexProgressBar?.isHidden = target == 0 && !complete
        indexProgressIcon?.image = WeftTheme.menuImage
        let summary = "\(remainingText) \(remainingValue)\n\(registeredText) \(added)개"
        indexingMenuItem?.title = fields?["scan_complete"] == "true" && indexMode == "watch" ? "파일 변경 감시 중 · 최근 등록 \(added)개" : summary.replacingOccurrences(of: "\n", with: " · ")
        let progressHint = target > 0 ? "진행 막대는 이전 인덱스의 파일 수를 기준으로 한 예상 진행률입니다." : "첫 인덱싱은 전체 파일 수를 알 수 없어 진행 막대를 숨깁니다."
        statusItem?.button?.toolTip = "Weft · \(summary)\n\(progressHint)"
    }
    func statusLine(title: String, value: String, valueColor: NSColor) -> NSAttributedString {
        let titleAttributes: [NSAttributedString.Key: Any] = [
            .font: NSFont.systemFont(ofSize: 8.5, weight: .medium),
            .foregroundColor: NSColor.white.withAlphaComponent(0.72)
        ]
        let valueAttributes: [NSAttributedString.Key: Any] = [
            .font: NSFont.systemFont(ofSize: 8.5, weight: .semibold),
            .foregroundColor: valueColor
        ]
        let result = NSMutableAttributedString(string: title + (value.isEmpty ? "" : "  "), attributes: titleAttributes)
        if !value.isEmpty { result.append(NSAttributedString(string: value, attributes: valueAttributes)) }
        return result
    }
    func indexLogProgressFields() -> [String: String]? {
        let url = db.deletingLastPathComponent().appendingPathComponent("desktop-index.log")
        guard let file = try? FileHandle(forReadingFrom: url) else { return nil }
        defer { try? file.close() }
        guard let end = try? file.seekToEnd(), end > indexLogStartOffset else { return nil }
        let start = max(indexLogStartOffset, end > 8192 ? end - 8192 : 0)
        guard (try? file.seek(toOffset: start)) != nil,
              let data = try? file.readToEnd(), let text = String(data: data, encoding: .utf8) else { return nil }
        for line in text.components(separatedBy: .newlines).reversed() where line.contains("scanned=") {
            var values: [String: String] = [:]
            for field in line.split(whereSeparator: \.isWhitespace) {
                let pair = field.split(separator: "=", maxSplits: 1)
                if pair.count == 2 { values[String(pair[0])] = String(pair[1]) }
            }
            if values["scanned"] != nil && values["indexed"] != nil { return values }
        }
        return nil
    }
    func indexLogProgressSummary() -> String? {
        guard let values = indexLogProgressFields(), let scanned = values["scanned"], let indexed = values["indexed"] else { return nil }
        return "인덱싱 · 확인 \(scanned) · 반영 \(indexed) · 건너뜀 \(values["unchanged/skipped"] ?? "0")"
    }
    func resumeIndexing() {
        guard !folder.isEmpty else { return }
        if UserDefaults.standard.bool(forKey: "indexingEnabled") && indexMode != "manual" {
            if indexMode == "interval" { scheduleIndexing() }
            if indexMode == "watch" || UserDefaults.standard.bool(forKey: "indexAtLaunch") { startIndexing(watch: indexMode == "watch") }
        } else if UserDefaults.standard.bool(forKey: "indexAtLaunch") { startIndexing(watch: false) }
    }
    func scheduleIndexing() {
        indexTimer?.invalidate()
        indexTimer = Timer.scheduledTimer(withTimeInterval: indexInterval, repeats: true) { [weak self] _ in
            guard let self = self, self.indexProcess?.isRunning != true, !self.panel.isVisible else { return }
            self.startIndexing(watch: false)
        }
    }
    func stopIndexing() {
        UserDefaults.standard.set(false, forKey: "indexingEnabled")
        indexTimer?.invalidate(); indexTimer = nil
        if indexProcess?.isRunning == true { indexProcess?.terminate() }
        updateIndexControls()
    }
    @objc func refreshIndexNow() {
        guard indexProcess?.isRunning != true else { indexingLabel?.stringValue = "이미 갱신 또는 변경 감시가 실행 중입니다."; return }
        startIndexing(watch: false)
    }
    func startIndexing(watch: Bool) {
        guard indexProcess?.isRunning != true else { return }
        guard !folder.isEmpty else { chooseFolder(); return }
        guard !indexTypes.isEmpty else { indexingLabel?.stringValue = "파일 형식을 하나 이상 선택하세요."; return }
        let process = Process(); process.executableURL = engine
        process.arguments = ["--db", db.path, "index", "--all", folder, "--prune", "--duty-cycle", String(indexDuty), "--file-types", indexTypes.joined(separator: ",")]
        if watch { process.arguments?.append("--watch") }
        if UserDefaults.standard.bool(forKey: "indexMediaContent") { process.arguments?.append("--media") }
        do {
            let directory = db.deletingLastPathComponent()
            try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
            let log = directory.appendingPathComponent("desktop-index.log")
            if !FileManager.default.fileExists(atPath: log.path) { FileManager.default.createFile(atPath: log.path, contents: nil) }
            let handle = try FileHandle(forWritingTo: log); try handle.seekToEnd(); logHandle = handle
            indexLogStartOffset = handle.offsetInFile
            process.standardOutput = handle; process.standardError = handle; process.qualityOfService = .background
            process.terminationHandler = { [weak self] process in
                DispatchQueue.main.async {
                    guard let self = self, self.indexProcess === process else { return }
                    if process.terminationStatus == 0,
                       let scanned = self.indexLogProgressFields().flatMap({ Int($0["scanned"] ?? "") }), scanned > 0 {
                        let target = UserDefaults.standard.integer(forKey: "indexProgressTarget")
                        UserDefaults.standard.set(max(target, scanned), forKey: "indexProgressTarget")
                    }
                    self.indexProcess = nil; try? self.logHandle?.close(); self.logHandle = nil
                    if watch { UserDefaults.standard.set(false, forKey: "indexingEnabled") }
                    self.updateIndexControls()
                    if process.terminationStatus == 0 { self.indexingLabel?.stringValue = "갱신 완료 · " + DateFormatter.localizedString(from: Date(), dateStyle: .short, timeStyle: .short) }
                    else if process.terminationReason != .uncaughtSignal { self.indexingLabel?.stringValue = "갱신 실패 · 로그에서 원인을 확인하세요." }
                }
            }
            try process.run(); indexProcess = process; updateIndexControls()
        } catch { indexingLabel?.stringValue = "시작 실패: " + error.localizedDescription }
    }
    @objc func prepareMedia() {
        guard indexProcess?.isRunning != true, mediaSetupProcess?.isRunning != true else { return }
        let alert = NSAlert(); alert.messageText = "미디어 내용 분석 환경을 준비할까요?"
        alert.informativeText = "Python과 미디어 처리 라이브러리가 필요합니다. 추가 다운로드와 저장 공간을 사용하며, 전체 모델은 미디어를 처음 인덱싱할 때 다운로드됩니다."
        alert.addButton(withTitle: "준비"); alert.addButton(withTitle: "취소")
        guard alert.runModal() == .alertFirstButtonReturn else { return }
        let process = Process(); process.executableURL = URL(fileURLWithPath: "/bin/sh")
        process.arguments = [Bundle.main.resourceURL!.appendingPathComponent("setup-media.sh").path]
        let handle = appLogHandle(); process.standardOutput = handle ?? FileHandle.nullDevice; process.standardError = handle ?? FileHandle.nullDevice
        process.terminationHandler = { [weak self] process in
            try? handle?.close()
            DispatchQueue.main.async {
                self?.mediaSetupProcess = nil; self?.updateIndexControls()
                self?.indexingLabel?.stringValue = process.terminationStatus == 0 ? "환경 준비 완료 · 미디어 내용 분석을 켤 수 있습니다." : "준비 실패 · Python 설치 여부와 검색 오류 로그를 확인하세요."
            }
        }
        do { try process.run(); mediaSetupProcess = process; updateIndexControls() }
        catch { indexingLabel?.stringValue = error.localizedDescription }
    }
    @objc func deleteIndex() {
        guard FileManager.default.fileExists(atPath: db.path) else { indexingLabel?.stringValue = "삭제할 인덱스가 없습니다."; return }
        let alert = NSAlert(); alert.messageText = "인덱스를 휴지통으로 이동할까요?"
        alert.informativeText = "원본 파일은 유지됩니다. 검색 데이터와 벡터 캐시를 함께 휴지통으로 이동하고 자동 갱신을 중지합니다. 다시 검색하려면 인덱싱해야 합니다."
        alert.addButton(withTitle: "휴지통으로 이동"); alert.addButton(withTitle: "취소")
        guard alert.runModal() == .alertFirstButtonReturn else { return }
        let active = indexProcess; stopIndexing(); cancelSearch(); clearInspector()
        UserDefaults.standard.set(false, forKey: "indexAtLaunch")
        (indexConfigurationControls.first(where: { $0.tag == 4 }) as? NSButton)?.state = .off
        let daemon = daemonProcess; if daemon?.isRunning == true { daemon?.terminate() }
        DispatchQueue.global(qos: .utility).async { [weak self] in
            guard let self = self else { return }
            if active?.isRunning == true { active?.waitUntilExit() }
            if daemon?.isRunning == true { daemon?.waitUntilExit() }
            var leases: [Int32] = []
            for name in ["index.lock", "watch.lock"] {
                let url = self.db.deletingPathExtension().appendingPathExtension(name)
                let fd = open(url.path, O_WRONLY | O_CREAT, S_IRUSR | S_IWUSR)
                guard fd >= 0, flock(fd, LOCK_EX | LOCK_NB) == 0 else {
                    if fd >= 0 { close(fd) }
                    for held in leases { _ = flock(held, LOCK_UN); close(held) }
                    DispatchQueue.main.async { self.indexingLabel?.stringValue = "다른 인덱싱 또는 변경 감시가 실행 중입니다. 그 작업을 중지한 뒤 다시 시도하세요." }
                    return
                }
                leases.append(fd)
            }
            defer { for fd in leases { _ = flock(fd, LOCK_UN); close(fd) } }
            let archive = self.db.deletingLastPathComponent().appendingPathComponent("Weft-index-\(UUID().uuidString)")
            var moved: [URL] = []
            do {
                try FileManager.default.createDirectory(at: archive, withIntermediateDirectories: false)
                for suffix in ["", "-wal", "-shm", ".vectors.bin"] {
                    let url = URL(fileURLWithPath: self.db.path + suffix)
                    if FileManager.default.fileExists(atPath: url.path) {
                        try FileManager.default.moveItem(at: url, to: archive.appendingPathComponent(url.lastPathComponent)); moved.append(url)
                    }
                }
                try FileManager.default.trashItem(at: archive, resultingItemURL: nil)
                DispatchQueue.main.async { self.hits = []; self.table.reloadData(); self.resizePanel(); self.indexingLabel?.stringValue = "인덱스를 휴지통으로 이동했습니다." }
            } catch {
                for url in moved { try? FileManager.default.moveItem(at: archive.appendingPathComponent(url.lastPathComponent), to: url) }
                try? FileManager.default.removeItem(at: archive)
                DispatchQueue.main.async { self.indexingLabel?.stringValue = "인덱스를 이동하지 못했습니다: " + error.localizedDescription }
            }
        }
    }

    func uninstallButton() -> NSButton {
        let button = NSButton(title: "Weft 제거…", target: self, action: #selector(uninstallWeft))
        button.hasDestructiveAction = true
        return button
    }

    static func isWeftMyvecShim(at url: URL) -> Bool {
        guard let contents = try? String(contentsOf: url, encoding: .utf8) else { return false }
        return contents == "#!/bin/sh\nexec \"$(dirname -- \"$0\")/weft\" \"$@\"\n"
    }

    @objc func uninstallWeft() {
        let home = FileManager.default.homeDirectoryForCurrentUser
        let cli = home.appendingPathComponent(".cargo/bin/weft")
        let myvec = home.appendingPathComponent(".cargo/bin/myvec")
        let bundle = Bundle.main.bundleURL
        let dataRoot = db.deletingLastPathComponent()
        let hasIndex = [db, URL(fileURLWithPath: db.path + "-wal"), URL(fileURLWithPath: db.path + "-shm"),
                        URL(fileURLWithPath: db.path + ".vectors.bin")].contains { FileManager.default.fileExists(atPath: $0.path) }
        let hasCLI = FileManager.default.fileExists(atPath: cli.path)
        let hasApp = FileManager.default.fileExists(atPath: bundle.path)
        let alert = NSAlert()
        alert.messageText = "Weft를 제거할까요?"
        alert.informativeText = "앱\(hasApp ? "" : " (없음)"), 로컬 인덱스\(hasIndex ? "" : " (없음)"), CLI\(hasCLI ? "" : " (없음)")를 휴지통으로 이동하고 자동 인덱싱을 중지합니다. 메모·녹음, 에이전트 스킬과 기존 백업은 보존됩니다."
        alert.addButton(withTitle: "Weft 제거")
        alert.addButton(withTitle: "취소")
        guard alert.runModal() == .alertFirstButtonReturn else { return }

        let activeIndex = indexProcess
        let activeDaemon = daemonProcess
        let activeVoiceIndex = voiceIndexProcess
        stopIndexing()
        cancelSearch()
        voiceTimer?.invalidate()
        if voiceCapture.isRecording, let recording = voiceCapture.stop() { try? FileManager.default.removeItem(at: recording) }
        if activeDaemon?.isRunning == true { activeDaemon?.terminate() }
        if activeIndex?.isRunning == true { activeIndex?.terminate() }
        if activeVoiceIndex?.isRunning == true { activeVoiceIndex?.terminate() }

        DispatchQueue.global(qos: .utility).async { [weak self] in
            guard let self = self else { return }
            if activeIndex?.isRunning == true { activeIndex?.waitUntilExit() }
            if activeDaemon?.isRunning == true { activeDaemon?.waitUntilExit() }
            if activeVoiceIndex?.isRunning == true { activeVoiceIndex?.waitUntilExit() }
            try? self.logHandle?.close()

            var leases: [Int32] = []
            for name in ["index.lock", "watch.lock"] {
                let lockURL = dataRoot.appendingPathComponent(name)
                let fd = open(lockURL.path, O_WRONLY | O_CREAT, S_IRUSR | S_IWUSR)
                guard fd >= 0, flock(fd, LOCK_EX | LOCK_NB) == 0 else {
                    if fd >= 0 { close(fd) }
                    for held in leases { _ = flock(held, LOCK_UN); close(held) }
                    DispatchQueue.main.async { self.presentUninstallResult("다른 인덱싱 작업이 아직 실행 중입니다. 앱과 데이터는 제거하지 않았습니다.", success: false) }
                    return
                }
                leases.append(fd)
            }
            defer { for fd in leases { _ = flock(fd, LOCK_UN); close(fd) } }

            do {
                let manager = FileManager.default
                let indexItems = [db, URL(fileURLWithPath: db.path + "-wal"), URL(fileURLWithPath: db.path + "-shm"),
                                  URL(fileURLWithPath: db.path + ".vectors.bin"), dataRoot.appendingPathComponent("desktop-index.log"),
                                  dataRoot.appendingPathComponent("desktop-embedding.sock")]
                let existingIndexItems = indexItems.filter { manager.fileExists(atPath: $0.path) }
                if !existingIndexItems.isEmpty {
                    let archive = dataRoot.appendingPathComponent("Weft-index-\(UUID().uuidString)")
                    try manager.createDirectory(at: archive, withIntermediateDirectories: false)
                    do {
                        for item in existingIndexItems { try manager.moveItem(at: item, to: archive.appendingPathComponent(item.lastPathComponent)) }
                        try manager.trashItem(at: archive, resultingItemURL: nil)
                    } catch {
                        for item in existingIndexItems {
                            let staged = archive.appendingPathComponent(item.lastPathComponent)
                            if manager.fileExists(atPath: staged.path) { try? manager.moveItem(at: staged, to: item) }
                        }
                        try? manager.removeItem(at: archive)
                        throw error
                    }
                }
                if manager.fileExists(atPath: cli.path) { try manager.trashItem(at: cli, resultingItemURL: nil) }
                if Self.isWeftMyvecShim(at: myvec) { try manager.trashItem(at: myvec, resultingItemURL: nil) }
                if manager.fileExists(atPath: bundle.path) { try manager.trashItem(at: bundle, resultingItemURL: nil) }

                DispatchQueue.main.async {
                    if let identifier = Bundle.main.bundleIdentifier { UserDefaults.standard.removePersistentDomain(forName: identifier) }
                    self.presentUninstallResult("Weft 앱, CLI, 인덱스를 휴지통으로 이동했습니다. 완전히 제거하려면 휴지통을 비우세요.", success: true)
                }
            } catch {
                DispatchQueue.main.async { self.presentUninstallResult("일부 항목을 이동하지 못했습니다: \(error.localizedDescription)", success: false) }
            }
        }
    }

    func presentUninstallResult(_ message: String, success: Bool) {
        let alert = NSAlert()
        alert.messageText = success ? "Weft 제거 완료" : "Weft 제거를 마치지 못했습니다"
        alert.informativeText = message
        alert.addButton(withTitle: success ? "종료" : "확인")
        alert.runModal()
        if success { NSApp.terminate(nil) }
    }

    func agentIntegrationSettings() -> NSViewController {
        let home = FileManager.default.homeDirectoryForCurrentUser
        let codexSkill = home.appendingPathComponent(".codex/skills/weft-semantic-search")
        let claudeSkill = home.appendingPathComponent(".claude/skills/weft-semantic-search")
        let sharedSkill = home.appendingPathComponent(".agents/skills/weft-semantic-search")
        agentCodex.state = FileManager.default.fileExists(atPath: codexSkill.path) ? .on : .off
        agentClaude.state = FileManager.default.fileExists(atPath: claudeSkill.path) ? .on : .off
        agentShared.state = FileManager.default.fileExists(atPath: sharedSkill.path) ? .on : .off
        for checkbox in [agentCodex, agentClaude, agentShared] {
            checkbox.target = self
            checkbox.action = #selector(agentSelectionChanged(_:))
        }
        let install = NSButton(title: "선택한 에이전트에 설치", target: self, action: #selector(installAgentIntegration))
        WeftTheme.primary(install)
        let remove = NSButton(title: "선택한 에이전트에서 제거", target: self, action: #selector(removeAgentIntegration))
        agentStatus.stringValue = "에이전트 설정은 온보딩으로 이동하지 않고 여기서 관리합니다."
        let agents = NSStackView(views: [agentCodex, agentClaude, agentShared])
        agents.spacing = 16
        let actions = NSStackView(views: [install, remove])
        actions.spacing = 12
        return settingsPage(title: "에이전트", subtitle: "원하는 에이전트에 Weft 파일 검색 스킬을 설치하거나 제거합니다.", views: [
            agents, actions, agentStatus,
            NSTextField(wrappingLabelWithString: "설치는 검색 CLI와 선택한 에이전트용 스킬 파일을 배치합니다. 기존 스킬은 백업 폴더에 보관합니다.")
        ])
    }

    @objc func agentSelectionChanged(_ sender: NSButton) {
        agentStatus.stringValue = "Codex, Claude Code, 공용 폴더 중 관리할 대상을 선택하세요."
    }

    @objc func installAgentIntegration() {
        let home = FileManager.default.homeDirectoryForCurrentUser
        var targets: [URL] = []
        if agentCodex.state == .on { targets.append(home.appendingPathComponent(".codex/skills")) }
        if agentClaude.state == .on { targets.append(home.appendingPathComponent(".claude/skills")) }
        if agentShared.state == .on { targets.append(home.appendingPathComponent(".agents/skills")) }
        guard !targets.isEmpty else { agentStatus.stringValue = "설치할 대상을 하나 이상 선택하세요."; return }
        guard let resources = Bundle.main.resourceURL else { agentStatus.stringValue = "앱 리소스를 찾을 수 없습니다."; return }
        let source = resources.appendingPathComponent("weft-semantic-search")
        guard FileManager.default.fileExists(atPath: source.path) else { agentStatus.stringValue = "앱에 검색 스킬 파일이 포함되어 있지 않습니다."; return }
        do {
            let manager = FileManager.default
            let archive = home.appendingPathComponent(".local/share/myvec/skill-backups")
            try manager.createDirectory(at: archive, withIntermediateDirectories: true)
            let cli = home.appendingPathComponent(".cargo/bin/weft")
            try manager.createDirectory(at: cli.deletingLastPathComponent(), withIntermediateDirectories: true)
            let stagedCLI = cli.deletingLastPathComponent().appendingPathComponent(".weft-\(UUID().uuidString)")
            try manager.copyItem(at: engine, to: stagedCLI)
            if manager.fileExists(atPath: cli.path) {
                try manager.moveItem(at: cli, to: archive.appendingPathComponent("weft-cli-\(UUID().uuidString)"))
            }
            try manager.moveItem(at: stagedCLI, to: cli)
            for target in targets {
                try manager.createDirectory(at: target, withIntermediateDirectories: true)
                let destination = target.appendingPathComponent("weft-semantic-search")
                if manager.fileExists(atPath: destination.path) {
                    try manager.moveItem(at: destination, to: archive.appendingPathComponent("skill-\(UUID().uuidString)"))
                }
                try manager.copyItem(at: source, to: destination)
            }
            agentStatus.stringValue = "설치했습니다. 에이전트를 다시 열면 Weft 검색 스킬을 사용할 수 있습니다."
        } catch {
            agentStatus.stringValue = "설치 실패: \(error.localizedDescription)"
        }
    }

    @objc func removeAgentIntegration() {
        let home = FileManager.default.homeDirectoryForCurrentUser
        var targets: [URL] = []
        if agentCodex.state == .on { targets.append(home.appendingPathComponent(".codex/skills/weft-semantic-search")) }
        if agentClaude.state == .on { targets.append(home.appendingPathComponent(".claude/skills/weft-semantic-search")) }
        if agentShared.state == .on { targets.append(home.appendingPathComponent(".agents/skills/weft-semantic-search")) }
        guard !targets.isEmpty else { agentStatus.stringValue = "제거할 대상을 하나 이상 선택하세요."; return }
        let alert = NSAlert()
        alert.messageText = "선택한 에이전트 스킬을 제거할까요?"
        alert.informativeText = "Weft CLI와 인덱스는 유지됩니다."
        alert.addButton(withTitle: "제거")
        alert.addButton(withTitle: "취소")
        guard alert.runModal() == .alertFirstButtonReturn else { return }
        do {
            for target in targets where FileManager.default.fileExists(atPath: target.path) {
                try FileManager.default.removeItem(at: target)
            }
            agentStatus.stringValue = "선택한 위치에서 스킬을 제거했습니다."
        } catch { agentStatus.stringValue = "제거 실패: \(error.localizedDescription)" }
    }

    func usageSettings() -> NSViewController {
        let rows = [
            ("파일 이름·경로 검색", "@file", "이름과 경로로 파일 찾기"),
            ("폴더 검색", "@folder", "폴더 결과 보기"),
            ("본문 청크 검색", "@chunk", "문서 내용에서 일치하는 부분 찾기"),
            ("웹 검색", "@web", "검색어를 입력하고 Enter를 누르면 기본 브라우저에서 검색"),
            ("설정 열기", "@설정", "Enter를 눌러 Weft 설정 창 열기"),
            ("음성 녹음·전사", "@녹음", "Enter를 눌러 녹음 시작 · 완료하면 전사문을 검색창에 입력"),
            ("마크다운 메모", "@메모", "Enter를 눌러 메모 편집기를 열고 저장하면 검색 가능"),
            ("사용자 바로가기", "별칭 + 검색어", "설정 → 바로가기에서 앱 또는 URL을 등록해 키보드로 실행"),
            ("여러 주제 함께 반영", "+ · @all", "의미 검색에서 두 주제를 함께 고려"),
            ("주제 중 하나 선택", "| · @any", "의미 검색에서 어느 한쪽과 맞는 결과 찾기"),
            ("피하고 싶은 주제", "- · @avoid", "해당 주제와 가까운 결과의 점수를 낮춤"),
            ("검색 방식 바꾸기", "Tab", "파일명 검색과 의미 검색 전환"),
            ("결과 이동", "↑ ↓", "목록을 이동하면 오른쪽에 미리보기 표시"),
            ("Finder에서 보기", "⌥ Return", "원본 파일 위치 표시"),
            ("미리보기", "⌘ Y", "macOS 미리보기 열기")
        ]
        let table = NSStackView()
        table.orientation = .vertical
        table.alignment = .leading
        table.spacing = 0
        func row(_ values: [String], header: Bool = false) -> NSView {
            let widths: [CGFloat] = [190, 105, 295]
            let cells = values.enumerated().map { index, value -> NSTextField in
                let cell = header ? NSTextField(labelWithString: value) : NSTextField(wrappingLabelWithString: value)
                cell.font = .systemFont(ofSize: header ? 11 : 12, weight: header ? .semibold : .regular)
                cell.textColor = header ? .secondaryLabelColor : .labelColor
                cell.maximumNumberOfLines = 2
                cell.widthAnchor.constraint(equalToConstant: widths[index]).isActive = true
                return cell
            }
            let line = NSStackView(views: cells)
            line.orientation = .horizontal
            line.alignment = .centerY
            line.spacing = 12
            line.heightAnchor.constraint(greaterThanOrEqualToConstant: header ? 28 : 38).isActive = true
            return line
        }
        table.addArrangedSubview(row(["풀어쓰기", "약칭", "기능"], header: true))
        for item in rows { table.addArrangedSubview(row([item.0, item.1, item.2])) }
        table.widthAnchor.constraint(equalToConstant: 626).isActive = true
        return settingsPage(title: "사용법", subtitle: "검색창에서 키보드만으로 범위와 검색 방식을 바꿀 수 있습니다.", views: [
            table,
            NSTextField(wrappingLabelWithString: "예: @file 보고서 · Tab을 누르면 의미 검색으로 전환 · 데이터베이스 + 재시도 - 테스트")
        ])
    }

    func permissionSettings() -> NSViewController {
        let choose = NSButton(title: "검색 폴더 선택…", target: self, action: #selector(chooseFolder))
        WeftTheme.primary(choose)
        return settingsPage(title: "권한", subtitle: "Weft는 검색에 필요한 범위에서만 파일을 읽습니다.", views: [
            settingsRow("파일 접근", control: choose),
            NSTextField(wrappingLabelWithString: "인덱싱 설정에서 폴더를 선택하면 해당 위치를 대상으로 사용합니다. macOS가 보호된 폴더의 접근을 막으면 폴더를 다시 선택해 권한을 허용하세요."),
            NSTextField(wrappingLabelWithString: "손쉬운 사용, 입력 모니터링, 마이크 권한은 필요하지 않습니다. Weft는 음성 녹음이나 전사를 하지 않습니다."),
            NSTextField(wrappingLabelWithString: "전체 디스크 접근 권한도 기본적으로 필요하지 않습니다. 접근이 거부된 파일은 인덱싱 로그에 표시됩니다.")
        ])
    }
}
