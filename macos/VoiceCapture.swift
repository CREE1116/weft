import AVFoundation
import Foundation

final class VoiceCapture {
    enum CaptureError: LocalizedError {
        case permissionDenied
        case invalidInput
        var errorDescription: String? {
            switch self {
            case .permissionDenied: return L("마이크 접근이 허용되지 않았습니다. 시스템 설정에서 Weft의 마이크 권한을 켜주세요.")
            case .invalidInput: return L("마이크 입력을 사용할 수 없습니다.")
            }
        }
    }

    private let engine = AVAudioEngine()
    private var output: AVAudioFile?
    private var converter: AVAudioConverter?
    private var recordingURL: URL?
    private(set) var isRecording = false

    func start(completion: @escaping (Result<Void, Error>) -> Void) {
        let begin: () -> Void = { [weak self] in
            guard let self else { return }
            do {
                let input = self.engine.inputNode
                let inputFormat = input.inputFormat(forBus: 0)
                guard inputFormat.channelCount > 0,
                      let format = AVAudioFormat(commonFormat: .pcmFormatFloat32, sampleRate: 16_000, channels: 1, interleaved: false),
                      let converter = AVAudioConverter(from: inputFormat, to: format) else { throw CaptureError.invalidInput }
                let url = FileManager.default.temporaryDirectory.appendingPathComponent("weft-voice-\(UUID().uuidString).caf")
                self.output = try AVAudioFile(forWriting: url, settings: format.settings)
                self.recordingURL = url
                self.converter = converter
                input.installTap(onBus: 0, bufferSize: 4096, format: inputFormat) { [weak self] buffer, _ in
                    guard let self else { return }
                    let capacity = AVAudioFrameCount(Double(buffer.frameLength) * 16_000 / inputFormat.sampleRate + 512)
                    guard let converted = AVAudioPCMBuffer(pcmFormat: format, frameCapacity: capacity) else { return }
                    var supplied = false
                    var conversionError: NSError?
                    let status = converter.convert(to: converted, error: &conversionError) { _, inputStatus in
                        if supplied { inputStatus.pointee = .noDataNow; return nil }
                        supplied = true
                        inputStatus.pointee = .haveData
                        return buffer
                    }
                    do {
                        if status == .error { throw conversionError ?? CaptureError.invalidInput }
                        if converted.frameLength > 0 { try self.output?.write(from: converted) }
                    } catch {
                        DispatchQueue.main.async {
                            completion(.failure(error))
                            _ = self.stop()
                        }
                    }
                }
                self.engine.prepare()
                try self.engine.start()
                self.isRecording = true
                completion(.success(()))
            } catch {
                self.engine.inputNode.removeTap(onBus: 0)
                self.output = nil
                completion(.failure(error))
            }
        }
        switch AVCaptureDevice.authorizationStatus(for: .audio) {
        case .authorized: begin()
        case .notDetermined:
            AVCaptureDevice.requestAccess(for: .audio) { granted in
                DispatchQueue.main.async { granted ? begin() : completion(.failure(CaptureError.permissionDenied)) }
            }
        default: completion(.failure(CaptureError.permissionDenied))
        }
    }

    func stop() -> URL? {
        guard isRecording else { return nil }
        engine.inputNode.removeTap(onBus: 0)
        engine.stop()
        output = nil
        converter = nil
        isRecording = false
        defer { recordingURL = nil }
        return recordingURL
    }
}
