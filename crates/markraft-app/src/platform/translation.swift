// The public system translation presentation is exposed only through SwiftUI.
// Keep that API behind a small C boundary; Rust owns the editor snapshot and
// decides whether a returned replacement still applies to the document.
import AppKit
import SwiftUI
import Translation

private typealias Completion = @convention(c) (UnsafeMutableRawPointer?, UnsafePointer<CChar>?) -> Void

@available(macOS 14.4, *)
@MainActor
private final class TranslationModel: ObservableObject {
    @Published var presented = false
    var completed: ((String?) -> Void)?
}

@available(macOS 14.4, *)
private struct TranslationContent: View {
    @ObservedObject var model: TranslationModel
    let text: String
    let editable: Bool
    let anchorSize: NSSize

    var body: some View {
        Color.clear
            .frame(width: anchorSize.width, height: anchorSize.height)
            .translationPresentation(
                isPresented: $model.presented,
                text: text,
                replacementAction: editable ? {
                    model.completed?($0)
                } : nil
            )
            .onChange(of: model.presented) { presented in
                if !presented {
                    model.completed?(nil)
                }
            }
    }
}

@available(macOS 14.4, *)
@MainActor
private final class TranslationAnchorView: NSHostingView<TranslationContent> {
    // The transparent anchor can cover selected text. Let pointer events reach
    // the editor; the translation popover handles input in its own window.
    override func hitTest(_ point: NSPoint) -> NSView? { nil }
}

@available(macOS 14.4, *)
@MainActor
private final class TranslationSession {
    private let model = TranslationModel()
    private var host: NSView?
    private var completion: Completion?
    private let context: UnsafeMutableRawPointer?
    private var finished = false

    init(view: NSView, parent: NativePresentationRoot, anchor: NSRect, text: String, editable: Bool,
         context: UnsafeMutableRawPointer?, completion: @escaping Completion) {
        self.context = context
        self.completion = completion
        let frame = parent.convert(anchor, from: view)
        let content = TranslationContent(
            model: model, text: text, editable: editable, anchorSize: frame.size
        )
        let host = TranslationAnchorView(rootView: content)
        // The hosting view supplies only the selection anchor. The system owns
        // the translation popover and its layout in a separate native window.
        host.sizingOptions = []
        host.frame = frame
        self.host = host
        parent.addSubview(host)
        model.completed = { [weak self] text in self?.finish(text) }
        // SwiftUI must first attach its hosting view to the existing key window.
        DispatchQueue.main.async { [weak self] in
            guard let self, !self.finished else { return }
            self.model.presented = true
        }
    }

    func finish(_ text: String?) {
        guard !finished else { return }
        finished = true
        let callback = completion
        completion = nil
        model.completed = nil
        model.presented = false
        // Removing the view in a SwiftUI update can invalidate its presentation
        // transaction. Detach after this event finishes, including cancellation.
        let oldHost = host
        host = nil
        DispatchQueue.main.async { oldHost?.removeFromSuperview() }
        if let text {
            text.withCString { callback?(context, $0) }
        } else {
            callback?(context, nil)
        }
    }
}

@_cdecl("markraft_translation_available")
func translationAvailable() -> Bool {
    if #available(macOS 14.4, *) { return true }
    return false
}

@_cdecl("markraft_translation_begin")
@MainActor
func translationBegin(
    _ viewPointer: UnsafeMutableRawPointer?, _ x: Double, _ y: Double,
    _ width: Double, _ height: Double,
    _ text: UnsafePointer<CChar>?, _ editable: Bool,
    _ context: UnsafeMutableRawPointer?,
    _ callback: @escaping @convention(c) (UnsafeMutableRawPointer?, UnsafePointer<CChar>?) -> Void
) -> UnsafeMutableRawPointer? {
    guard #available(macOS 14.4, *), let viewPointer, let text else { return nil }
    let view = Unmanaged<NSView>.fromOpaque(viewPointer).takeUnretainedValue()
    guard let parent = NativePresentationRoot.containing(view) else { return nil }
    let session = TranslationSession(
        view: view, parent: parent, anchor: NSRect(x: x, y: y, width: width, height: height),
        text: String(cString: text),
        editable: editable, context: context, completion: callback
    )
    return Unmanaged.passRetained(session).toOpaque()
}

@_cdecl("markraft_translation_cancel")
@MainActor
func translationCancel(_ pointer: UnsafeMutableRawPointer?) {
    guard #available(macOS 14.4, *), let pointer else { return }
    let session = Unmanaged<TranslationSession>.fromOpaque(pointer).takeRetainedValue()
    session.finish(nil)
}
