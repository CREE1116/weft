import Cocoa

enum WeftTheme {
    static let accent = NSColor(name: NSColor.Name("WeftPurple")) { appearance in
        appearance.bestMatch(from: [.darkAqua, .aqua]) == .darkAqua
            ? NSColor(srgbRed: 0.68, green: 0.55, blue: 0.94, alpha: 1)
            : NSColor(srgbRed: 0.43, green: 0.28, blue: 0.77, alpha: 1)
    }
    static func primary(_ button: NSButton) {
        button.bezelStyle = .rounded
        button.bezelColor = NSColor(srgbRed: 0.44, green: 0.29, blue: 0.78, alpha: 1)
        button.contentTintColor = .white
        button.font = .systemFont(ofSize: 12, weight: .medium)
    }
    /// One horizontal weft alternates over/under the four strokes of W.
    static func drawMark(in rect: NSRect, monochrome: Bool = false) {
        NSGraphicsContext.saveGraphicsState()
        let transform = NSAffineTransform()
        transform.translateX(by: rect.minX, yBy: rect.minY)
        transform.scaleX(by: rect.width / 100, yBy: rect.height / 100)
        transform.concat()
        let w = NSBezierPath()
        for (i, point) in [NSPoint(x: 20, y: 76), NSPoint(x: 34, y: 24), NSPoint(x: 50, y: 55), NSPoint(x: 66, y: 24), NSPoint(x: 80, y: 76)].enumerated() {
            if i == 0 { w.move(to: point) } else { w.line(to: point) }
        }
        w.lineWidth = 8; w.lineCapStyle = .round; w.lineJoinStyle = .round
        func mask(_ crossings: [CGFloat], radius: CGFloat) {
            let mask = NSBezierPath(rect: NSRect(x: -10, y: -10, width: 120, height: 120))
            mask.windingRule = .evenOdd
            for x in crossings { mask.appendOval(in: NSRect(x: x - radius, y: 48 - radius, width: radius * 2, height: radius * 2)) }
            mask.addClip()
        }
        NSGraphicsContext.saveGraphicsState()
        if !monochrome { mask([46.4, 72.5], radius: 5) }
        (monochrome ? NSColor.black : NSColor(srgbRed: 0.98, green: 0.97, blue: 1, alpha: 1)).setStroke(); w.stroke()
        NSGraphicsContext.restoreGraphicsState()
        NSGraphicsContext.saveGraphicsState()
        if !monochrome { mask([27.5, 53.6], radius: 6) }
        let weft = NSBezierPath(); weft.move(to: NSPoint(x: 14, y: 48)); weft.line(to: NSPoint(x: 86, y: 48))
        weft.lineWidth = monochrome ? 4 : 5; weft.lineCapStyle = .round
        (monochrome ? NSColor.black : NSColor(srgbRed: 0.83, green: 0.77, blue: 1, alpha: 1)).setStroke(); weft.stroke()
        NSGraphicsContext.restoreGraphicsState()
        NSGraphicsContext.restoreGraphicsState()
    }
    static var menuImage: NSImage {
        let image = NSImage(size: NSSize(width: 22, height: 22))
        image.lockFocus(); drawMark(in: NSRect(x: -1, y: -1, width: 24, height: 24), monochrome: true); image.unlockFocus()
        image.isTemplate = true
        return image
    }
}
