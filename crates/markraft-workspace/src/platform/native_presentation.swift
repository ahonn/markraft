import AppKit

// Keep native presentations outside the editor's virtual accessibility tree.
// GPUI's AccessKit adapter remains attached to its original content view; this
// ordinary AppKit container exposes that view and native hosts as siblings.
// The window owns this boundary for its lifetime, independently of any session.
@MainActor
final class NativePresentationRoot: NSView {
    static func containing(_ view: NSView) -> NativePresentationRoot? {
        guard let window = view.window, let content = window.contentView else { return nil }
        if let root = content as? NativePresentationRoot { return root }

        let responder = window.firstResponder
        let root = NativePresentationRoot(frame: content.frame)
        window.contentView = root
        content.frame = root.bounds
        content.autoresizingMask = [.width, .height]
        root.addSubview(content)
        // Changing a window's content view may clear its first responder.
        // Restore the existing editor responder, not a second text input view.
        if let responder { window.makeFirstResponder(responder) }
        return root
    }
}
