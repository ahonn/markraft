// Run on an English macOS installation:
// swift crates/markraft-workspace/src/platform/context_menu/fixtures/captions.swift > crates/markraft-workspace/src/platform/context_menu/fixtures/captions.json
// Synthetic, offscreen NSTextView menus only; no popup, clipboard or preferences.
import AppKit

let app = NSApplication.shared
let window = NSWindow(contentRect: NSRect(x: 0, y: 0, width: 600, height: 400),
                      styleMask: [.titled], backing: .buffered, defer: false)
let view = NSTextView(frame: NSRect(x: 0, y: 0, width: 600, height: 400))
window.contentView = view
let samples = [
    "Paragraph style QA\nSecond paragraph.",
    "one two three four five six seven eight nine ten",
    "The quick brown fox jumps over the lazy dog",
    "   hello\t\nworld   ", "one\n\ntwo", "  a  b  ", "a\t\tb",
    "a\r\nb", "a\u{000B}b", "a\u{000C}b", "a\u{0085}b",
    "a\u{2028}b", "a\u{2029}b", "a\u{2003}b", "a\u{00A0}b",
    "a\u{2007}b", "a\u{202F}b", "a\u{200B}b",
    "abc-def-ghi-jkl-mno-pqr-stu-vwx-yz",
    "abc/def/ghi/jkl/mno/pqr/stu/vwx/yz",
    "abc,def,ghi,jkl,mno,pqr,stu,vwx,yz",
    "abc—def—ghi—jkl—mno—pqr—stu—vwx—yz",
    "这是一个用于比较原生右键菜单标题截断规则的中文字符串及更多文字",
    String(repeating: "a", count: 30), String(repeating: "a", count: 31),
    String(repeating: "e\u{301}", count: 20),
    String(repeating: "👨‍👩‍👧‍👦", count: 5),
    "12345678901234567890123456789😀X", String(repeating: "😀", count: 15) + "X",
    "a" + String(repeating: " ", count: 40) + "b",
    "e" + String(repeating: "\u{301}", count: 40) + " more",
] + (23...30).map { String(repeating: "a", count: $0) + " don't stop" }
  + (23...30).map { String(repeating: "甲", count: $0) + "这是一个测试句子" }
  + (23...30).map { String(repeating: "a", count: $0) + " xyz abcdef" }

var cases: [[String]] = []
for sample in samples {
    view.string = sample
    view.setSelectedRange(NSRange(location: 0, length: (sample as NSString).length))
    let event = NSEvent.mouseEvent(with: .rightMouseDown, location: NSPoint(x: 8, y: 380),
                                  modifierFlags: [], timestamp: 0, windowNumber: window.windowNumber,
                                  context: nil, eventNumber: 0, clickCount: 1, pressure: 1)!
    let title = view.menu(for: event)!.items.first { $0.title.hasPrefix("Look Up “") }!.title
    let caption = String(title.dropFirst("Look Up “".count).dropLast())
    cases.append([sample, caption])
}
let data = try JSONSerialization.data(withJSONObject: cases, options: [.prettyPrinted, .sortedKeys])
print(String(decoding: data, as: UTF8.self))
