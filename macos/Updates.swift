import Cocoa
import CryptoKit

struct AppRelease: Decodable {
    struct Asset: Decodable {
        let name: String
        let browser_download_url: String
        let digest: String?
        let size: Int64
    }
    let tag_name: String
    let prerelease: Bool
    let draft: Bool
    let assets: [Asset]
    var version: String { tag_name.hasPrefix("v") ? String(tag_name.dropFirst()) : tag_name }
    func archive(for architecture: String) -> Asset? {
        assets.first { $0.name == "Weft-\(version)-\(architecture).zip" && $0.size > 0 && $0.size < 300_000_000 }
    }
    static func validArchivePaths(_ entries: String) -> Bool {
        let names = entries.split(separator: "\n")
        return !names.isEmpty && names.allSatisfy {
            !$0.hasPrefix("/") && !$0.split(separator: "/").contains("..")
                && ($0.hasPrefix("Weft.app/") || $0.hasPrefix("__MACOSX/"))
        }
    }
    static func newer(_ candidate: String, than current: String) -> Bool {
        func parts(_ value: String) -> [Int]? {
            let parts = value.split(separator: ".", omittingEmptySubsequences: false)
            guard (1...4).contains(parts.count) else { return nil }
            let numbers = parts.compactMap { Int($0) }
            guard numbers.count == parts.count, numbers.allSatisfy({ $0 >= 0 }) else { return nil }
            return numbers + Array(repeating: 0, count: 4 - numbers.count)
        }
        guard let new = parts(candidate), let old = parts(current) else { return false }
        return old.lexicographicallyPrecedes(new)
    }
}

final class AppUpdater {
    unowned let appDelegate: AppDelegate
    let architecture: String = {
        #if arch(arm64)
        return "arm64"
        #else
        return "x86_64"
        #endif
    }()
    var timer: Timer?
    var status = "업데이트 확인 전" { didSet { statusLabel?.stringValue = status } }
    weak var statusLabel: NSTextField?
    weak var installButton: NSButton?
    var release: AppRelease?
    var busy = false
    var staged: URL?
    var session = URLSession(configuration: .ephemeral)
    var autoCheck: Bool { UserDefaults.standard.object(forKey: "updateAutoCheck") as? Bool ?? true }
    var autoInstall: Bool { UserDefaults.standard.object(forKey: "updateAutoInstall") as? Bool ?? true }
    var currentVersion: String { Bundle.main.object(forInfoDictionaryKey: "CFBundleShortVersionString") as? String ?? "1.0.0" }
    init(app: AppDelegate) { appDelegate = app }
    func start() {
        timer = Timer.scheduledTimer(withTimeInterval: 30, repeats: true) { [weak self] _ in self?.tick() }
        tick()
    }
    func tick() {
        if staged != nil && autoInstall && idle { installStaged(); return }
        guard autoCheck && !busy else { return }
        let last = UserDefaults.standard.double(forKey: "updateLastCheck")
        if Date().timeIntervalSince1970 - last >= 12 * 3600 { check() }
    }
    var idle: Bool {
        !appDelegate.panel.isVisible && appDelegate.settingsWindow?.isVisible != true
            && appDelegate.onboarding?.window?.isVisible != true && appDelegate.indexProcess?.isRunning != true
            && appDelegate.mediaSetupProcess?.isRunning != true
    }
    func check() {
        guard !busy else { return }
        busy = true; status = "GitHub 릴리스 확인 중…"
        var request = URLRequest(url: URL(string: "https://api.github.com/repos/CREE1116/weft/releases/latest")!)
        request.timeoutInterval = 20
        request.setValue("application/vnd.github+json", forHTTPHeaderField: "Accept")
        request.setValue("Weft/\(currentVersion)", forHTTPHeaderField: "User-Agent")
        session.dataTask(with: request) { [weak self] data, response, error in
            DispatchQueue.main.async {
                guard let self = self else { return }
                self.busy = false
                UserDefaults.standard.set(Date().timeIntervalSince1970, forKey: "updateLastCheck")
                if let error = error { self.status = "연결 실패 · " + error.localizedDescription; return }
                guard let response = response as? HTTPURLResponse, response.statusCode == 200,
                      let data = data, let release = try? JSONDecoder().decode(AppRelease.self, from: data) else {
                    self.status = "릴리스를 확인할 수 없습니다. 잠시 후 다시 확인하세요."; return
                }
                guard !release.draft, !release.prerelease, AppRelease.newer(release.version, than: self.currentVersion), release.archive(for: self.architecture) != nil else {
                    self.status = "설치 가능한 새 앱 릴리스가 없습니다. (현재 \(self.currentVersion))"; return
                }
                self.release = release; self.installButton?.isEnabled = true
                self.status = "Weft \(release.version) 업데이트가 있습니다."
                if self.autoInstall { self.download() }
            }
        }.resume()
    }
    func download() {
        guard !busy, let release = release, let asset = release.archive(for: architecture),
              let url = URL(string: asset.browser_download_url), url.scheme == "https", url.host == "github.com",
              url.path.hasPrefix("/CREE1116/weft/releases/download/") else { return }
        busy = true; status = "업데이트 다운로드 중…"; installButton?.isEnabled = false
        session.downloadTask(with: url) { [weak self] location, response, error in
            guard let self = self else { return }
            do {
                if let error = error { throw error }
                guard let location = location, let response = response as? HTTPURLResponse, response.statusCode == 200 else { throw self.failure("업데이트 다운로드에 실패했습니다.") }
                let directory = FileManager.default.temporaryDirectory.appendingPathComponent("weft-update-\(UUID().uuidString)")
                try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true, attributes: [.posixPermissions: 0o700])
                let archive = directory.appendingPathComponent("update.zip")
                try FileManager.default.moveItem(at: location, to: archive)
                let bytes = try Data(contentsOf: archive, options: .mappedIfSafe)
                let hash = SHA256.hash(data: bytes).map { String(format: "%02x", $0) }.joined()
                // Releases must provide the server's asset digest or the generated checksums asset.
                let expected: String
                if let digest = asset.digest, digest.hasPrefix("sha256:") { expected = String(digest.dropFirst(7)) }
                else {
                    guard let checksum = release.assets.first(where: { $0.name == "SHA256SUMS" }),
                          let checksumURL = URL(string: checksum.browser_download_url), checksumURL.scheme == "https", checksumURL.host == "github.com", checksumURL.path.hasPrefix("/CREE1116/weft/releases/download/") else { throw self.failure("검증 파일이 없는 업데이트는 설치하지 않습니다.") }
                    let sums = try String(contentsOf: checksumURL, encoding: .utf8)
                    guard let line = sums.split(separator: "\n").first(where: { $0.split(whereSeparator: { $0.isWhitespace }).last.map { String($0).replacingOccurrences(of: "*", with: "").split(separator: "/").last == Substring(asset.name) } ?? false }),
                          let value = line.split(whereSeparator: { $0.isWhitespace }).first else { throw self.failure("업데이트 체크섬을 찾을 수 없습니다.") }
                    expected = String(value)
                }
                guard hash == expected.lowercased() else { throw self.failure("업데이트 파일 검증에 실패했습니다.") }
                let entries = try self.run("/usr/bin/unzip", ["-Z1", archive.path])
                guard AppRelease.validArchivePaths(entries) else { throw self.failure("업데이트 압축 파일의 경로가 올바르지 않습니다.") }
                _ = try self.run("/usr/bin/ditto", ["-x", "-k", archive.path, directory.path])
                let candidate = directory.appendingPathComponent("Weft.app")
                _ = try self.run("/usr/bin/codesign", ["--verify", "--deep", "--strict", candidate.path])
                _ = try self.run("/usr/bin/codesign", ["--verify", "--strict", candidate.appendingPathComponent("Contents/Resources/weft").path])
                guard let bundle = Bundle(url: candidate), bundle.bundleIdentifier == Bundle.main.bundleIdentifier,
                      bundle.object(forInfoDictionaryKey: "CFBundleShortVersionString") as? String == release.version else { throw self.failure("업데이트 앱의 버전 또는 식별자가 일치하지 않습니다.") }
                DispatchQueue.main.async {
                    self.busy = false; self.staged = candidate; self.installButton?.isEnabled = true
                    self.status = "업데이트 준비 완료 · 검색과 인덱싱이 끝나면 설치합니다."
                    if self.autoInstall && self.idle { self.installStaged() }
                }
            } catch {
                DispatchQueue.main.async { self.busy = false; self.installButton?.isEnabled = true; self.status = "업데이트 실패 · " + error.localizedDescription }
            }
        }.resume()
    }
    func failure(_ message: String) -> Error { NSError(domain: "WeftUpdater", code: 1, userInfo: [NSLocalizedDescriptionKey: message]) }
    func run(_ executable: String, _ arguments: [String]) throws -> String {
        let process = Process(); process.executableURL = URL(fileURLWithPath: executable); process.arguments = arguments
        let output = Pipe(); process.standardOutput = output; process.standardError = FileHandle.nullDevice
        try process.run(); let data = output.fileHandleForReading.readDataToEndOfFile(); process.waitUntilExit()
        guard process.terminationStatus == 0 else { throw failure("업데이트 확인 도구 실행 실패") }
        return String(data: data, encoding: .utf8) ?? ""
    }
    func installStaged() {
        guard let staged = staged, !busy else { return }
        let destination = Bundle.main.bundleURL
        guard FileManager.default.isWritableFile(atPath: destination.path), FileManager.default.isWritableFile(atPath: destination.deletingLastPathComponent().path) else {
            status = "앱을 쓰기 가능한 응용프로그램 폴더로 옮긴 뒤 다시 시도하세요."; return
        }
        guard appDelegate.indexProcess?.isRunning != true else { status = "인덱싱이 끝나면 설치합니다."; return }
        func quote(_ value: String) -> String { "'" + value.replacingOccurrences(of: "'", with: "'\\''") + "'" }
        let directory = staged.deletingLastPathComponent()
        let backup = destination.deletingLastPathComponent().appendingPathComponent(".Weft-previous-\(UUID().uuidString).app")
        let script = directory.appendingPathComponent("install.sh")
        let body = """
        #!/bin/sh
        set -eu
        attempt=0
        while kill -0 \(ProcessInfo.processInfo.processIdentifier) 2>/dev/null; do
          attempt=$((attempt + 1))
          if [ "$attempt" -gt 150 ]; then exit 1; fi
          sleep 0.2
        done
        if ! /bin/mv \(quote(destination.path)) \(quote(backup.path)); then
          /usr/bin/open \(quote(destination.path))
          exit 1
        fi
        if /bin/mv \(quote(staged.path)) \(quote(destination.path)) && /usr/bin/open \(quote(destination.path)); then
          :
        else
          /bin/rm -rf \(quote(destination.path))
          /bin/mv \(quote(backup.path)) \(quote(destination.path))
          /usr/bin/open \(quote(destination.path))
          exit 1
        fi
        """
        do {
            try body.write(to: script, atomically: true, encoding: .utf8)
            try FileManager.default.setAttributes([.posixPermissions: 0o700], ofItemAtPath: script.path)
            let helper = Process(); helper.executableURL = URL(fileURLWithPath: "/bin/sh"); helper.arguments = [script.path]
            helper.standardOutput = FileHandle.nullDevice; helper.standardError = FileHandle.nullDevice
            try helper.run(); busy = true; status = "업데이트를 설치하고 다시 시작합니다."
            appDelegate.quit()
        } catch { status = "업데이트 설치 실패 · " + error.localizedDescription }
    }
}

extension AppDelegate {
    func updateSettings() -> NSViewController {
        let check = NSButton(checkboxWithTitle: "GitHub에서 새 버전 자동 확인 (12시간 간격)", target: self, action: #selector(updatePreferenceChanged(_:)))
        check.tag = 1; check.state = updater?.autoCheck == false ? .off : .on
        let install = NSButton(checkboxWithTitle: "업데이트를 자동 다운로드하고 사용하지 않을 때 설치", target: self, action: #selector(updatePreferenceChanged(_:)))
        install.tag = 2; install.state = updater?.autoInstall == false ? .off : .on
        let status = NSTextField(wrappingLabelWithString: updater?.status ?? "업데이트 확인 전")
        status.font = .systemFont(ofSize: 12); status.textColor = .secondaryLabelColor; updater?.statusLabel = status
        let checkNow = NSButton(title: "지금 확인", target: self, action: #selector(checkForUpdates))
        let installNow = NSButton(title: "업데이트 설치", target: self, action: #selector(installUpdate))
        WeftTheme.primary(installNow)
        installNow.isEnabled = updater?.release != nil; updater?.installButton = installNow
        let buttons = NSStackView(views: [checkNow, installNow]); buttons.spacing = 12
        return settingsPage(title: "업데이트", subtitle: "CREE1116/weft의 정식 앱 릴리스를 사용합니다.", views: [check, install,
            NSTextField(wrappingLabelWithString: "다운로드 파일의 SHA-256과 앱 서명을 확인합니다. 검색·인덱싱 중에는 자동 설치를 기다리며, 설치 후 앱을 다시 엽니다. 기존 인덱스와 설정은 유지됩니다."), status, buttons])
    }
    @objc func updatePreferenceChanged(_ sender: NSButton) {
        UserDefaults.standard.set(sender.state == .on, forKey: sender.tag == 1 ? "updateAutoCheck" : "updateAutoInstall")
    }
    @objc func checkForUpdates() { updater?.check() }
    @objc func installUpdate() { if updater?.staged != nil { updater?.installStaged() } else { updater?.download() } }
}
