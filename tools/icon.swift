// Draws pmux's app icon: `swift tools/icon.swift out.png [size]`.
// "Stacked holds": terminal windows stacked on a warm gradient, the front one with a prompt,
// for the sessions `pmux hold` keeps alive behind the window.
import AppKit

let args = CommandLine.arguments
let out = args.count > 1 ? args[1] : "icon.png"
let size = args.count > 2 ? CGFloat(Double(args[2]) ?? 1024) : 1024

let rep = NSBitmapImageRep(bitmapDataPlanes: nil, pixelsWide: Int(size), pixelsHigh: Int(size), bitsPerSample: 8,
                           samplesPerPixel: 4, hasAlpha: true, isPlanar: false, colorSpaceName: .deviceRGB,
                           bytesPerRow: 0, bitsPerPixel: 0)!
NSGraphicsContext.saveGraphicsState()
NSGraphicsContext.current = NSGraphicsContext(bitmapImageRep: rep)
let s = size / 1024

func rgb(_ r: CGFloat, _ g: CGFloat, _ b: CGFloat, _ a: CGFloat = 1) -> NSColor {
    NSColor(calibratedRed: r, green: g, blue: b, alpha: a)
}
/// Rounded rect in 1024-unit icon coordinates.
func rr(_ x: CGFloat, _ y: CGFloat, _ w: CGFloat, _ h: CGFloat, _ r: CGFloat) -> NSBezierPath {
    NSBezierPath(roundedRect: NSRect(x: x * s, y: y * s, width: w * s, height: h * s), xRadius: r * s, yRadius: r * s)
}

// macOS icon grid: 824pt body centred in 1024 with a soft shadow.
let shadow = NSShadow()
shadow.shadowColor = NSColor.black.withAlphaComponent(0.35)
shadow.shadowBlurRadius = 24 * s
shadow.shadowOffset = NSSize(width: 0, height: -10 * s)
NSGraphicsContext.saveGraphicsState()
shadow.set()
NSGradient(starting: rgb(1.0, 0.62, 0.30), ending: rgb(0.93, 0.30, 0.38))!.draw(in: rr(100, 100, 824, 824, 185), angle: -90)
NSGraphicsContext.restoreGraphicsState()

// Three windows, back to front, each more opaque.
for i in 0..<3 {
    let o = CGFloat(i) * 70
    rgb(0.10, 0.10, 0.13, i == 2 ? 1 : 0.35 + 0.2 * CGFloat(i)).setFill()
    rr(220 + o, 520 - o, 460, 300, 44).fill()
}

// Prompt `>_` in the front window.
let chevron = NSBezierPath()
chevron.move(to: NSPoint(x: 410 * s, y: 470 * s))
chevron.line(to: NSPoint(x: 450 * s, y: 435 * s))
chevron.line(to: NSPoint(x: 410 * s, y: 400 * s))
chevron.lineWidth = 22 * s
chevron.lineCapStyle = .round
chevron.lineJoinStyle = .round
NSColor.white.setStroke()
chevron.stroke()
NSColor(white: 1, alpha: 0.9).setFill()
rr(480, 395, 120, 22, 11).fill()

NSGraphicsContext.restoreGraphicsState()
try! rep.representation(using: .png, properties: [:])!.write(to: URL(fileURLWithPath: out))
