import Cocoa
import SwiftUI

final class MemoDocument: ObservableObject {
    @Published var text = "# " {
        didSet { onChange?(text) }
    }
    @Published var wantsFocus = false
    var onChange: ((String) -> Void)?
}

private struct MemoMarkdownEditor: View {
    @ObservedObject var document: MemoDocument

    var body: some View {
        var configuration = MarkdownEditorConfiguration.default
        configuration.textInsets = TextInsets(horizontal: 18, vertical: 18)
        configuration.theme.headingMarker = WeftTheme.accent
        return NativeTextViewWrapper(
            text: $document.text,
            configuration: configuration,
            fontSize: 14,
            documentId: "weft-memo",
            isFocused: $document.wantsFocus
        )
    }
}

extension AppDelegate {
    func storageDirectory(for tag: Int) -> URL {
        let key = [1: "voiceRecordingDirectory", 2: "voiceTranscriptDirectory", 3: "memoDirectory"][tag]!
        if let value = UserDefaults.standard.string(forKey: key), !value.isEmpty {
            return URL(fileURLWithPath: NSString(string: value).expandingTildeInPath, isDirectory: true)
        }
        let child = [1: "Weft Voice Notes/Audio", 2: "Weft Voice Notes/Transcripts", 3: "Weft Notes"][tag]!
        return FileManager.default.homeDirectoryForCurrentUser.appendingPathComponent("Documents/\(child)", isDirectory: true)
    }

    func memoAndVoiceSettings() -> NSViewController {
        memoPathLabels.removeAll()
        let audio = storagePathControl(tag: 1)
        let transcripts = storagePathControl(tag: 2)
        let notes = storagePathControl(tag: 3)
        let explanation = NSTextField(wrappingLabelWithString: L("@녹음으로 녹음을 시작하고 Enter를 누르면 저장합니다. @메모는 마크다운 편집기를 엽니다. 전사문과 메모는 저장한 파일만 개별 인덱싱해 검색할 수 있으며, 녹음 파일은 인덱싱하지 않습니다."))
        explanation.font = .systemFont(ofSize: 12)
        explanation.textColor = .secondaryLabelColor
        return settingsPage(title: L("음성·메모"), subtitle: L("음성 녹음, 전사문, 직접 작성한 메모의 저장 위치를 정합니다."), views: [
            settingsSection(L("저장 위치")),
            settingsRow(L("음성 녹음"), control: audio),
            settingsRow(L("음성 전사"), control: transcripts),
            settingsRow(L("마크다운 메모"), control: notes),
            explanation
        ])
    }

    func storagePathControl(tag: Int) -> NSView {
        let path = NSTextField(labelWithString: storageDirectory(for: tag).path)
        path.font = .systemFont(ofSize: 11)
        path.textColor = .secondaryLabelColor
        path.lineBreakMode = .byTruncatingMiddle
        path.toolTip = path.stringValue
        memoPathLabels[tag] = path
        let choose = NSButton(title: L("변경…"), target: self, action: #selector(chooseStorageDirectory(_:)))
        choose.tag = tag
        let control = NSStackView(views: [path, choose])
        control.orientation = .horizontal
        control.alignment = .centerY
        control.spacing = 8
        path.setContentCompressionResistancePriority(.defaultLow, for: .horizontal)
        choose.widthAnchor.constraint(equalToConstant: 76).isActive = true
        return control
    }

    @objc func chooseStorageDirectory(_ sender: NSButton) {
        let picker = NSOpenPanel()
        picker.title = L("저장 폴더 선택")
        picker.message = L("저장 폴더를 선택하세요. 필요한 폴더는 저장할 때 만듭니다.")
        picker.canChooseFiles = false
        picker.canChooseDirectories = true
        picker.allowsMultipleSelection = false
        picker.directoryURL = storageDirectory(for: sender.tag)
        guard picker.runModal() == .OK, let url = picker.url else { return }
        let key = [1: "voiceRecordingDirectory", 2: "voiceTranscriptDirectory", 3: "memoDirectory"][sender.tag]!
        UserDefaults.standard.set(url.path, forKey: key)
        if let label = memoPathLabels[sender.tag] {
            label.stringValue = url.path
            label.toolTip = url.path
        }
    }

    @objc func createMemoFromMenu() { openMemoEditor(title: "") }

    func openMemoEditor(title: String) {
        hide()
        if memoWindow == nil { buildMemoWindow() }
        memoDocument?.text = title.isEmpty ? "# " : "# \(title)\n\n"
        memoDocument?.wantsFocus = false
        memoWindow?.title = title.isEmpty ? L("새 메모") : title
        NSApp.activate(ignoringOtherApps: true)
        memoWindow?.center()
        memoWindow?.makeKeyAndOrderFront(nil)
        DispatchQueue.main.async { [weak self] in
            self?.memoDocument?.wantsFocus = true
            DispatchQueue.main.asyncAfter(deadline: .now() + 0.05) {
                guard let self, let textView = self.memoTextView(in: self.memoEditorView) else { return }
                textView.setSelectedRange(NSRange(location: min(2, (textView.string as NSString).length), length: 0))
            }
        }
    }

    func buildMemoWindow() {
        let window = NSWindow(contentRect: NSRect(x: 0, y: 0, width: 700, height: 560),
            styleMask: [.titled, .closable, .miniaturizable, .resizable], backing: .buffered, defer: false)
        window.title = L("새 메모")
        window.isReleasedWhenClosed = false
        window.delegate = self
        let document = MemoDocument()
        document.onChange = { [weak window] text in
            let title = text.split(separator: "\n", maxSplits: 1, omittingEmptySubsequences: false).first.map(String.init)?
                .replacingOccurrences(of: "^#{1,6}\\s*", with: "", options: .regularExpression) ?? ""
            window?.title = title.isEmpty ? L("새 메모") : String(title.prefix(48))
        }
        let editor = NSHostingView(rootView: MemoMarkdownEditor(document: document))
        editor.translatesAutoresizingMaskIntoConstraints = false
        let status = NSTextField(labelWithString: L("첫 줄은 제목 · Markdown 자동 서식 · ⌘S 저장"))
        status.font = .systemFont(ofSize: 11)
        status.textColor = .secondaryLabelColor
        let cancel = NSButton(title: L("취소"), target: self, action: #selector(cancelMemo))
        let save = NSButton(title: L("저장"), target: self, action: #selector(saveMemo))
        save.keyEquivalent = "\r"
        WeftTheme.primary(save)
        for view in [status, cancel, save] { view.translatesAutoresizingMaskIntoConstraints = false }
        window.contentView?.addSubview(editor)
        window.contentView?.addSubview(status)
        window.contentView?.addSubview(cancel)
        window.contentView?.addSubview(save)
        NSLayoutConstraint.activate([
            editor.leadingAnchor.constraint(equalTo: window.contentView!.leadingAnchor),
            editor.trailingAnchor.constraint(equalTo: window.contentView!.trailingAnchor),
            editor.topAnchor.constraint(equalTo: window.contentView!.topAnchor),
            editor.bottomAnchor.constraint(equalTo: status.topAnchor, constant: -12),
            status.leadingAnchor.constraint(equalTo: window.contentView!.leadingAnchor, constant: 22),
            status.bottomAnchor.constraint(equalTo: window.contentView!.bottomAnchor, constant: -16),
            cancel.trailingAnchor.constraint(equalTo: save.leadingAnchor, constant: -8),
            cancel.centerYAnchor.constraint(equalTo: status.centerYAnchor),
            save.trailingAnchor.constraint(equalTo: window.contentView!.trailingAnchor, constant: -22),
            save.centerYAnchor.constraint(equalTo: status.centerYAnchor),
            save.widthAnchor.constraint(equalToConstant: 86)
        ])
        memoWindow = window
        memoDocument = document
        memoEditorView = editor
    }

    func memoTextView(in view: NSView?) -> NSTextView? {
        guard let view else { return nil }
        if let textView = view as? NSTextView { return textView }
        for child in view.subviews {
            if let textView = memoTextView(in: child) { return textView }
        }
        return nil
    }

    @objc func cancelMemo() { memoWindow?.orderOut(nil) }

    @objc func saveMemo() {
        guard let document = memoDocument else { return }
        let source = document.text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !source.isEmpty else { NSSound.beep(); return }
        let lines = source.split(separator: "\n", maxSplits: 1, omittingEmptySubsequences: false)
        let title = String(lines.first ?? "")
            .replacingOccurrences(of: "^#{1,6}\\s*", with: "", options: .regularExpression)
            .trimmingCharacters(in: .whitespacesAndNewlines)
        let body = lines.count > 1 ? String(lines[1]).trimmingCharacters(in: .whitespacesAndNewlines) : ""
        let displayTitle = title.isEmpty ? L("메모") : title
        let safeTitle = displayTitle.unicodeScalars.map { scalar -> Character in
            CharacterSet.alphanumerics.contains(scalar) || scalar == " " || scalar == "-" || scalar == "_"
                ? Character(scalar) : "_"
        }
        .reduce(into: "") { $0.append($1) }
        .trimmingCharacters(in: .whitespacesAndNewlines)
        let stamp = ISO8601DateFormatter().string(from: Date()).replacingOccurrences(of: ":", with: "-").prefix(16)
        let file = storageDirectory(for: 3).appendingPathComponent("\(safeTitle.isEmpty ? "메모" : safeTitle)-\(stamp).md")
        do {
            try FileManager.default.createDirectory(at: file.deletingLastPathComponent(), withIntermediateDirectories: true)
            let content = title.isEmpty ? source + "\n" : "# \(title)\n\n\(body)\n"
            try content.write(to: file, atomically: true, encoding: .utf8)
            memoWindow?.orderOut(nil)
            showPanel()
            search.stringValue = title.isEmpty ? L("메모") : title
            controlTextDidChange(Notification(name: NSControl.textDidChangeNotification))
            hint.stringValue = L("메모 저장됨 · 검색 인덱싱 중…")
            indexNoteFile(file, completionMessage: L("메모 저장 및 검색 인덱싱 완료"))
        } catch {
            let alert = NSAlert(); alert.messageText = L("메모를 저장하지 못했습니다."); alert.informativeText = error.localizedDescription
            alert.addButton(withTitle: L("확인")); alert.runModal()
        }
    }

}
