import AVFoundation
import Foundation
import SherpaOnnx

struct Segment: Encodable {
    let start: Double
    let end: Double
    let speaker: Int
    let text: String
}
struct Transcript: Encodable { let language: String; let text: String; let segments: [Segment] }

func fail(_ message: String) -> Never {
    FileHandle.standardError.write(Data((message + "\n").utf8))
    exit(2)
}

guard CommandLine.arguments.count == 5 else { fail("usage: weft-speech AUDIO ASR_DIR DIARIZATION_DIR EMBEDDING_MODEL") }
let audioURL = URL(fileURLWithPath: CommandLine.arguments[1])
let asrDirectory = URL(fileURLWithPath: CommandLine.arguments[2])
let diarizationDirectory = URL(fileURLWithPath: CommandLine.arguments[3])
let embeddingPath = CommandLine.arguments[4]

do {
    let file = try AVAudioFile(forReading: audioURL)
    let sourceFormat = file.processingFormat
    let targetFormat = AVAudioFormat(commonFormat: .pcmFormatFloat32, sampleRate: 16_000, channels: 1, interleaved: false)!
    let sourceCapacity = AVAudioFrameCount(file.length)
    guard let source = AVAudioPCMBuffer(pcmFormat: sourceFormat, frameCapacity: sourceCapacity) else { fail("Unable to allocate audio buffer") }
    try file.read(into: source)
    let samples: [Float]
    if abs(sourceFormat.sampleRate - 16_000) < 1, sourceFormat.channelCount == 1,
       sourceFormat.commonFormat == .pcmFormatFloat32, let pointer = source.floatChannelData?.pointee {
        samples = Array(UnsafeBufferPointer(start: pointer, count: Int(source.frameLength)))
    } else {
        guard let converter = AVAudioConverter(from: sourceFormat, to: targetFormat) else { fail("Unable to convert audio to mono 16 kHz PCM") }
        let targetCapacity = AVAudioFrameCount(Double(source.frameLength) * 16_000 / sourceFormat.sampleRate + 4096)
        guard let target = AVAudioPCMBuffer(pcmFormat: targetFormat, frameCapacity: targetCapacity) else { fail("Unable to allocate converted audio buffer") }
        var supplied = false
        var conversionError: NSError?
        let status = converter.convert(to: target, error: &conversionError) { _, inputStatus in
            if supplied { inputStatus.pointee = .noDataNow; return nil }
            supplied = true
            inputStatus.pointee = .haveData
            return source
        }
        guard status != .error, conversionError == nil, let pointer = target.floatChannelData?.pointee else {
            fail("Audio conversion failed: \(conversionError?.localizedDescription ?? "unknown error")")
        }
        samples = Array(UnsafeBufferPointer(start: pointer, count: Int(target.frameLength)))
    }
    guard !samples.isEmpty else { fail("Recording is empty") }

    let modelPath = asrDirectory.appendingPathComponent("model.int8.onnx").path
    let tokensPath = asrDirectory.appendingPathComponent("tokens.txt").path
    let sense = sherpaOnnxOfflineSenseVoiceModelConfig(model: modelPath, language: "auto", useInverseTextNormalization: true)
    let model = sherpaOnnxOfflineModelConfig(tokens: tokensPath, numThreads: 2, senseVoice: sense)
    var recognizerConfig = sherpaOnnxOfflineRecognizerConfig(featConfig: sherpaOnnxFeatureConfig(sampleRate: 16_000), modelConfig: model)
    let recognizer = SherpaOnnxOfflineRecognizer(config: &recognizerConfig)

    let segmentationPath = diarizationDirectory.appendingPathComponent("model.onnx").path
    var diarizationConfig = sherpaOnnxOfflineSpeakerDiarizationConfig(
        segmentation: sherpaOnnxOfflineSpeakerSegmentationModelConfig(
            pyannote: sherpaOnnxOfflineSpeakerSegmentationPyannoteModelConfig(model: segmentationPath, windowShiftRatio: 0.1), numThreads: 2),
        embedding: sherpaOnnxSpeakerEmbeddingExtractorConfig(model: embeddingPath, numThreads: 2),
        clustering: sherpaOnnxFastClusteringConfig(numClusters: 0, threshold: 0.5))
    let diarizer = SherpaOnnxOfflineSpeakerDiarizationWrapper(config: &diarizationConfig)
    let diarized = diarizer.process(samples: samples).filter { $0.end > $0.start }

    var segments: [Segment] = []
    var languages: [String] = []
    for item in diarized {
        let lower = max(0, min(samples.count, Int(Double(item.start) * 16_000)))
        let upper = max(lower, min(samples.count, Int(Double(item.end) * 16_000)))
        guard upper - lower > 1_600 else { continue }
        let result = recognizer.decode(samples: Array(samples[lower..<upper]), sampleRate: 16_000)
        let text = result.text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !text.isEmpty else { continue }
        languages.append(result.lang)
        segments.append(Segment(start: Double(item.start), end: Double(item.end), speaker: item.speaker + 1, text: text))
    }
    if segments.isEmpty {
        let result = recognizer.decode(samples: samples, sampleRate: 16_000)
        let text = result.text.trimmingCharacters(in: .whitespacesAndNewlines)
        if !text.isEmpty { segments = [Segment(start: 0, end: Double(samples.count) / 16_000, speaker: 1, text: text)]; languages = [result.lang] }
    }
    let language = Dictionary(grouping: languages, by: { $0 }).max(by: { $0.value.count < $1.value.count })?.key ?? "unknown"
    let fullText = segments.map(\.text).joined(separator: " ")
    let output = Transcript(language: language, text: fullText, segments: segments)
    let data = try JSONEncoder().encode(output)
    FileHandle.standardOutput.write(data)
    FileHandle.standardOutput.write(Data([0x0A]))
} catch { fail(error.localizedDescription) }
