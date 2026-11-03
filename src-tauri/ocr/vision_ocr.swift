import AppKit
import Vision

guard CommandLine.arguments.count == 2,
      let image = NSImage(contentsOfFile: CommandLine.arguments[1]),
      let cgImage = image.cgImage(forProposedRect: nil, context: nil, hints: nil) else {
  exit(2)
}

let request = VNRecognizeTextRequest { request, _ in
  let lines = (request.results as? [VNRecognizedTextObservation] ?? [])
    .compactMap { $0.topCandidates(1).first?.string }
  print(lines.joined(separator: "\n"))
}
request.recognitionLevel = .accurate
request.usesLanguageCorrection = true
request.recognitionLanguages = ["en-US"]
let handler = VNImageRequestHandler(cgImage: cgImage, options: [:])
do { try handler.perform([request]) } catch { exit(3) }
