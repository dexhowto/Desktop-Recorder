// Draws the DexHowTo Recorder app icon: a record button (white ring, red dot)
// above the letters "DR" on a dark rounded square, following the macOS icon grid.
//
// Usage: swift assets/make_icon.swift <output.png> [size]
// The default size is 1024. bundle.sh turns the 1024 PNG into AppIcon.icns.

import AppKit

let args = CommandLine.arguments
let outPath = args.count > 1 ? args[1] : "AppIcon-1024.png"
let size = args.count > 2 ? CGFloat(Double(args[2]) ?? 1024) : 1024
let k = size / 1024  // all coordinates below are on a 1024 canvas

func rgb(_ hex: UInt32, _ alpha: CGFloat = 1) -> NSColor {
    NSColor(
        srgbRed: CGFloat((hex >> 16) & 0xFF) / 255,
        green: CGFloat((hex >> 8) & 0xFF) / 255,
        blue: CGFloat(hex & 0xFF) / 255,
        alpha: alpha)
}

func circle(_ cx: CGFloat, _ cy: CGFloat, _ r: CGFloat) -> NSBezierPath {
    NSBezierPath(ovalIn: NSRect(x: (cx - r) * k, y: (cy - r) * k, width: 2 * r * k, height: 2 * r * k))
}

let rep = NSBitmapImageRep(
    bitmapDataPlanes: nil, pixelsWide: Int(size), pixelsHigh: Int(size),
    bitsPerSample: 8, samplesPerPixel: 4, hasAlpha: true, isPlanar: false,
    colorSpaceName: .deviceRGB, bytesPerRow: 0, bitsPerPixel: 0)!
NSGraphicsContext.saveGraphicsState()
NSGraphicsContext.current = NSGraphicsContext(bitmapImageRep: rep)
NSGraphicsContext.current?.imageInterpolation = .high

// ── Body: 824×824 rounded square centered on the canvas (macOS icon grid) ──
let body = NSRect(x: 100 * k, y: 100 * k, width: 824 * k, height: 824 * k)
let bodyPath = NSBezierPath(roundedRect: body, xRadius: 186 * k, yRadius: 186 * k)

NSGraphicsContext.saveGraphicsState()
let drop = NSShadow()
drop.shadowColor = NSColor.black.withAlphaComponent(0.38)
drop.shadowBlurRadius = 28 * k
drop.shadowOffset = NSSize(width: 0, height: -12 * k)
drop.set()
rgb(0x14161C).setFill()
bodyPath.fill()
NSGraphicsContext.restoreGraphicsState()

NSGradient(colors: [rgb(0x2E3446), rgb(0x1A1E29), rgb(0x0F1116)],
           atLocations: [0, 0.55, 1], colorSpace: .sRGB)!
    .draw(in: bodyPath, angle: -90)

// Soft blue-ish glow at the top for depth
NSGraphicsContext.saveGraphicsState()
bodyPath.addClip()
NSGradient(colors: [rgb(0x5888FF, 0.20), rgb(0x5888FF, 0)])!
    .draw(fromCenter: NSPoint(x: 512 * k, y: 900 * k), radius: 0,
          toCenter: NSPoint(x: 512 * k, y: 900 * k), radius: 520 * k, options: [])
NSGraphicsContext.restoreGraphicsState()

// Hairline highlight along the edge
rgb(0xFFFFFF, 0.10).setStroke()
let rim = NSBezierPath(roundedRect: body.insetBy(dx: 1.5 * k, dy: 1.5 * k),
                       xRadius: 185 * k, yRadius: 185 * k)
rim.lineWidth = 3 * k
rim.stroke()

// ── Record button: ring + glowing red dot ──
let cx: CGFloat = 512, cy: CGFloat = 610

// Faint dark plate behind the ring
rgb(0x000000, 0.22).setFill()
circle(cx, cy, 214).fill()

let ring = circle(cx, cy, 186)
ring.lineWidth = 30 * k
NSGradient(colors: [rgb(0xFFFFFF), rgb(0xD9DEE8)])!.draw(in: {
    // Gradient-filled ring: outer circle minus inner circle
    let p = circle(cx, cy, 201)
    p.append(circle(cx, cy, 171).reversed)
    return p
}(), angle: -90)

// Red glow
NSGraphicsContext.saveGraphicsState()
let glow = NSShadow()
glow.shadowColor = rgb(0xFF3B47, 0.75)
glow.shadowBlurRadius = 46 * k
glow.shadowOffset = .zero
glow.set()
rgb(0xE8283A).setFill()
circle(cx, cy, 128).fill()
NSGraphicsContext.restoreGraphicsState()

// Red dot body with a vertical gradient
NSGradient(colors: [rgb(0xFF6B6F), rgb(0xF0323F), rgb(0xC81E30)],
           atLocations: [0, 0.5, 1], colorSpace: .sRGB)!
    .draw(in: circle(cx, cy, 128), angle: -90)

// Glossy highlight on the upper part of the dot
NSGraphicsContext.saveGraphicsState()
circle(cx, cy, 128).addClip()
let gloss = NSBezierPath(ovalIn: NSRect(x: (cx - 92) * k, y: (cy + 18) * k, width: 184 * k, height: 104 * k))
NSGradient(colors: [rgb(0xFFFFFF, 0.55), rgb(0xFFFFFF, 0.0)])!.draw(in: gloss, angle: -90)
NSGraphicsContext.restoreGraphicsState()

// ── "DR" lettering ──
let font = NSFont.systemFont(ofSize: 220 * k, weight: .heavy)
let shadow = NSShadow()
shadow.shadowColor = NSColor.black.withAlphaComponent(0.35)
shadow.shadowBlurRadius = 10 * k
shadow.shadowOffset = NSSize(width: 0, height: -5 * k)
let attrs: [NSAttributedString.Key: Any] = [
    .font: font,
    .foregroundColor: NSColor.white,
    .kern: 10 * k,
    .shadow: shadow,
]
let text = NSAttributedString(string: "DR", attributes: attrs)
let textSize = text.size()
// Cap height sits between the ring and the bottom edge of the body
let capTop: CGFloat = 352 * k
let baselineY = capTop - font.capHeight
text.draw(at: NSPoint(x: (size - textSize.width) / 2 + 5 * k, y: baselineY + font.descender))

NSGraphicsContext.restoreGraphicsState()

let png = rep.representation(using: .png, properties: [:])!
try! png.write(to: URL(fileURLWithPath: outPath))
print("Wrote \(outPath) (\(Int(size))×\(Int(size)))")
