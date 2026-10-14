import Cocoa
import UniformTypeIdentifiers

enum UserShortcutKind: String, Codable {
    case app
    case url
}

struct UserShortcut: Codable, Identifiable, Equatable {
    var id: UUID
    var title: String
    var alias: String
    var kind: UserShortcutKind
    var target: String
}

extension AppDelegate {
    var userShortcuts: [UserShortcut] {
        get {
            guard let data = UserDefaults.standard.data(forKey: "userShortcuts") else { return [] }
            return (try? JSONDecoder().decode([UserShortcut].self, from: data)) ?? []
        }
        set {
            guard let data = try? JSONEncoder().encode(newValue) else { return }
            UserDefaults.standard.set(data, forKey: "userShortcuts")
        }
    }

    func quickLinkHits(for raw: String) -> [Hit]? {
        let value = raw.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !value.isEmpty, !userShortcuts.isEmpty else { return nil }
        let separator = value.rangeOfCharacter(from: .whitespacesAndNewlines)
        let typedAlias = separator.map { String(value[..<$0.lowerBound]) } ?? value
        let argument = separator.map { String(value[$0.upperBound...]).trimmingCharacters(in: .whitespacesAndNewlines) }
        let matches: [UserShortcut]
        if argument != nil {
            matches = userShortcuts.filter {
                $0.alias.caseInsensitiveCompare(typedAlias) == .orderedSame && ($0.kind == .url || argument?.isEmpty == true)
            }
        } else {
            matches = userShortcuts
                .filter { $0.alias.hasPrefix(typedAlias) || $0.alias.lowercased().hasPrefix(typedAlias.lowercased()) }
                .sorted { lhs, rhs in
                    let leftExact = lhs.alias.caseInsensitiveCompare(typedAlias) == .orderedSame
                    let rightExact = rhs.alias.caseInsensitiveCompare(typedAlias) == .orderedSame
                    return leftExact == rightExact ? lhs.title.localizedCaseInsensitiveCompare(rhs.title) == .orderedAscending : leftExact
                }
        }
        guard !matches.isEmpty else { return nil }
        return matches.map { item in
            Hit(path: item.id.uuidString, kind: "shortcut", score: 0, start_line: 0, end_line: 0, context: argument)
        }
    }

    func quickLinksSettings() -> NSViewController {
        let type = NSPopUpButton()
        type.addItems(withTitles: [L("앱 실행"), L("URL 열기")])
        type.target = self; type.action = #selector(quickLinkTypeChanged(_:))
        quickLinkType = type

        let title = NSTextField(); title.placeholderString = L("예: GitHub")
        quickLinkTitle = title
        let alias = NSTextField(); alias.placeholderString = L("예: gh")
        quickLinkAlias = alias
        let target = NSTextField(); target.placeholderString = L("앱을 선택하세요")
        quickLinkTarget = target
        let choose = NSButton(title: L("앱 선택…"), target: self, action: #selector(chooseQuickLinkApp))
        let targetControl = NSStackView(views: [target, choose])
        targetControl.orientation = .horizontal; targetControl.spacing = 8
        target.setContentCompressionResistancePriority(.defaultLow, for: .horizontal)
        targetControl.widthAnchor.constraint(equalToConstant: 460).isActive = true
        choose.widthAnchor.constraint(equalToConstant: 90).isActive = true

        let save = NSButton(title: L("바로가기 추가"), target: self, action: #selector(saveQuickLink))
        WeftTheme.primary(save)
        let status = NSTextField(wrappingLabelWithString: "")
        status.font = .systemFont(ofSize: 11); status.textColor = .secondaryLabelColor
        quickLinkStatus = status
        let rows = NSStackView(); rows.orientation = .vertical; rows.alignment = .leading; rows.spacing = 6
        quickLinkRows = rows
        refreshQuickLinkRows()

        let page = settingsPage(title: L("바로가기"), subtitle: L("앱이나 URL을 별칭으로 등록하고 검색창에서 바로 실행합니다."), views: [
            settingsRow(L("종류"), control: type), settingsRow(L("이름"), control: title),
            settingsRow(L("별칭"), control: alias), settingsRow(L("대상"), control: targetControl),
            NSTextField(wrappingLabelWithString: L("URL은 https://example.com/search?q={query}처럼 쓸 수 있습니다. 별칭 뒤에 검색어를 입력하면 {query} 자리에 들어갑니다. 앱은 ‘앱 선택’으로 등록합니다.")),
            save, status, settingsSection(L("내 바로가기")), rows
        ])
        return page
    }

    @objc func quickLinkTypeChanged(_ sender: NSPopUpButton) {
        let isApp = sender.indexOfSelectedItem == 0
        quickLinkTarget?.placeholderString = isApp ? L("앱을 선택하세요") : "https://example.com/search?q={query}"
        if let button = quickLinkTarget?.superview?.subviews.compactMap({ $0 as? NSButton }).first {
            button.isHidden = !isApp
        }
    }

    @objc func chooseQuickLinkApp() {
        let picker = NSOpenPanel()
        picker.title = L("바로가기에 등록할 앱을 선택하세요")
        picker.allowedContentTypes = [.applicationBundle]
        picker.allowsOtherFileTypes = false
        picker.canChooseFiles = true; picker.canChooseDirectories = false; picker.allowsMultipleSelection = false
        if picker.runModal() == .OK, let url = picker.url { quickLinkTarget?.stringValue = url.path }
    }

    @objc func saveQuickLink() {
        guard let titleField = quickLinkTitle, let aliasField = quickLinkAlias,
              let targetField = quickLinkTarget, let type = quickLinkType else { return }
        let title = titleField.stringValue.trimmingCharacters(in: .whitespacesAndNewlines)
        let alias = aliasField.stringValue.trimmingCharacters(in: .whitespacesAndNewlines)
        let target = targetField.stringValue.trimmingCharacters(in: .whitespacesAndNewlines)
        let kind: UserShortcutKind = type.indexOfSelectedItem == 0 ? .app : .url
        guard !title.isEmpty, !alias.isEmpty, !target.isEmpty,
              !alias.contains(where: { $0.isWhitespace }), !alias.hasPrefix("@") else {
            quickLinkStatus?.stringValue = L("이름·별칭·대상을 입력하세요. 별칭에는 공백이나 @를 사용할 수 없습니다.")
            return
        }
        if userShortcuts.contains(where: { $0.alias.caseInsensitiveCompare(alias) == .orderedSame }) {
            quickLinkStatus?.stringValue = L("이미 사용 중인 별칭입니다. 다른 별칭을 입력하세요.")
            return
        }
        if kind == .app {
            guard target.lowercased().hasSuffix(".app"), FileManager.default.fileExists(atPath: target) else {
                quickLinkStatus?.stringValue = L("설치된 .app을 선택하세요.")
                return
            }
        } else {
            let candidate = resolvedURL(for: target, query: "weft test")
            let scheme = candidate?.scheme?.lowercased()
            guard let scheme, !["file", "data", "javascript"].contains(scheme) else {
                quickLinkStatus?.stringValue = L("유효한 웹 URL 또는 앱 딥링크를 입력하세요.")
                return
            }
        }
        var all = userShortcuts
        all.append(UserShortcut(id: UUID(), title: title, alias: alias, kind: kind, target: target))
        userShortcuts = all
        titleField.stringValue = ""; aliasField.stringValue = ""; targetField.stringValue = ""
        quickLinkStatus?.stringValue = LF("바로가기를 추가했습니다. 검색창에서 ‘{0}’을 입력하세요.", String(describing: alias))
        refreshQuickLinkRows()
    }

    func refreshQuickLinkRows() {
        guard let rows = quickLinkRows else { return }
        rows.arrangedSubviews.forEach { rows.removeArrangedSubview($0); $0.removeFromSuperview() }
        let items = userShortcuts
        if items.isEmpty {
            let empty = NSTextField(labelWithString: L("등록된 바로가기가 없습니다."))
            empty.textColor = .secondaryLabelColor
            rows.addArrangedSubview(empty)
            return
        }
        for item in items {
            let title = NSTextField(labelWithString: "\(item.title)  ·  \(item.alias)")
            title.font = .systemFont(ofSize: 12, weight: .medium)
            title.lineBreakMode = .byTruncatingMiddle
            title.toolTip = item.target
            let remove = NSButton(title: L("삭제"), target: self, action: #selector(removeQuickLink(_:)))
            remove.identifier = NSUserInterfaceItemIdentifier(item.id.uuidString)
            remove.bezelStyle = .rounded
            let row = NSStackView(views: [title, remove]); row.orientation = .horizontal; row.spacing = 10
            title.setContentCompressionResistancePriority(.defaultLow, for: .horizontal)
            title.widthAnchor.constraint(equalToConstant: 350).isActive = true
            remove.widthAnchor.constraint(equalToConstant: 70).isActive = true
            row.widthAnchor.constraint(equalToConstant: 460).isActive = true
            rows.addArrangedSubview(row)
        }
    }

    @objc func removeQuickLink(_ sender: NSButton) {
        guard let rawID = sender.identifier?.rawValue, let id = UUID(uuidString: rawID) else { return }
        userShortcuts = userShortcuts.filter { $0.id != id }
        refreshQuickLinkRows()
    }

    func runQuickLink(_ item: UserShortcut, query: String) {
        let url = item.kind == .app ? URL(fileURLWithPath: item.target) : resolvedURL(for: item.target, query: query)
        guard let url, NSWorkspace.shared.open(url) else {
            hint.stringValue = L("바로가기를 열지 못했습니다. 앱이 설치되어 있는지 또는 URL을 확인하세요.")
            return
        }
        hide()
    }

    func resolvedURL(for template: String, query: String) -> URL? {
        let allowed = CharacterSet(charactersIn: "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789-._~")
        let encoded = query.addingPercentEncoding(withAllowedCharacters: allowed) ?? ""
        return URLComponents(string: template.replacingOccurrences(of: "{query}", with: encoded))?.url
    }
}
