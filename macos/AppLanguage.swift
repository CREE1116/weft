import Foundation

enum AppLanguage: String, CaseIterable {
    case ko
    case en
    case zhHans = "zh-Hans"
    case ja

    var nativeName: String {
        switch self {
        case .ko: return "한국어"
        case .en: return "English"
        case .zhHans: return "简体中文"
        case .ja: return "日本語"
        }
    }

    static let preferenceKey = "uiLanguage"

    static var current: AppLanguage {
        UserDefaults.standard.string(forKey: preferenceKey).flatMap(AppLanguage.init(rawValue:)) ?? .ko
    }

    static let languageChangedNotification = Notification.Name.weftLanguageChanged

    static func select(_ language: AppLanguage) {
        UserDefaults.standard.set(language.rawValue, forKey: preferenceKey)
        NotificationCenter.default.post(name: languageChangedNotification, object: language)
    }

    static var selected: AppLanguage { current }
    func select() { Self.select(self) }

    static func localize(_ key: String) -> String {
        localize(key, language: current)
    }

    static func localize(_ key: String, language: AppLanguage) -> String {
        guard language != .ko else { return key }
        return localizedBundle(for: language)?.localizedString(forKey: key, value: key, table: "Localizable") ?? key
    }

    private static func localizedBundle(for language: AppLanguage) -> Bundle? {
        // Resolve a specific language rather than following the Mac's preferred languages.
        if let path = Bundle.main.path(forResource: language.rawValue, ofType: "lproj"),
           let bundle = Bundle(path: path) {
            return bundle
        }
        guard let resources = Bundle.main.resourceURL else { return nil }
        return Bundle(url: resources.appendingPathComponent("Resources")
            .appendingPathComponent(language.rawValue + ".lproj"))
    }
}

extension Notification.Name {
    static let weftLanguageChanged = Notification.Name("weftLanguageChanged")
}

func L(_ key: String) -> String { AppLanguage.localize(key) }

func LF(_ key: String, _ arguments: String...) -> String {
    formatLocalizedTemplate(L(key), arguments: arguments)
}

func formatLocalizedTemplate(_ template: String, arguments: [String]) -> String {
    let result = NSMutableString(string: template)
    let expression = try! NSRegularExpression(pattern: "\\{([0-9]+)\\}")
    let matches = expression.matches(in: template, range: NSRange(location: 0, length: result.length))
    for match in matches.reversed() {
        guard let index = Int((template as NSString).substring(with: match.range(at: 1))),
              arguments.indices.contains(index) else { continue }
        result.replaceCharacters(in: match.range, with: arguments[index])
    }
    return result as String
}
