// Text recognition on the VM's display, for apps that expose no useful
// accessibility tree. Runs on the host (Vision), so like host capture it
// works before login, while locked and without any guest permission.
//
// Vision reports normalized boxes with a bottom-left origin; they leave here
// in display points with a top-left origin, the space input uses. Matching
// and rendering are the host core's job.

import CoreGraphics
import Foundation
import Vision
import DoppelWire

enum TextRecognizer {
    static func recognize(_ image: CGImage, displayPoints: CGSize) throws -> Doppel_Vmm_V1_RecognizedText {
        let request = VNRecognizeTextRequest()
        request.recognitionLevel = .accurate
        request.usesLanguageCorrection = true
        try VNImageRequestHandler(cgImage: image, options: [:]).perform([request])
        let space = Space(points: displayPoints)
        return .with {
            $0.displayPoints = .with { $0.width = displayPoints.width; $0.height = displayPoints.height }
            $0.lines = (request.results ?? []).compactMap { observation in
                guard let candidate = observation.topCandidates(1).first else { return nil }
                return .with { line in
                    line.text = candidate.string
                    line.frame = space.rect(observation.boundingBox)
                    line.confidence = candidate.confidence
                    line.words = words(of: candidate, in: space)
                }
            }
        }
    }

    /// Each word's own box, so a match can point at the words rather than
    /// the middle of a line that holds several controls.
    private static func words(of candidate: VNRecognizedText, in space: Space) -> [Doppel_Vmm_V1_TextWord] {
        let text = candidate.string
        var words: [Doppel_Vmm_V1_TextWord] = []
        text.enumerateSubstrings(in: text.startIndex..., options: .byWords) { word, range, _, _ in
            guard let word, let box = try? candidate.boundingBox(for: range) else { return }
            words.append(.with { $0.text = word; $0.frame = space.rect(box.boundingBox) })
        }
        return words
    }

    /// Normalized image space (bottom-left origin) → display points (top-left).
    private struct Space {
        let points: CGSize

        func rect(_ normalized: CGRect) -> Doppel_Common_V1_Rect {
            .with {
                $0.origin = .with {
                    $0.x = normalized.minX * points.width
                    $0.y = (1 - normalized.maxY) * points.height
                }
                $0.size = .with {
                    $0.width = normalized.width * points.width
                    $0.height = normalized.height * points.height
                }
            }
        }
    }
}
