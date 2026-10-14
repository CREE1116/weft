import Cocoa
import QuartzCore

/// Short-lived main-thread spring; no display-link or timer runs while idle.
final class WindowMotion {
    weak var window: NSWindow?
    var timer: Timer?
    var start: NSRect = .zero
    var target: NSRect = .zero
    var started: CFTimeInterval = 0
    let duration: CFTimeInterval = 0.45
    init(window: NSWindow) { self.window = window }
    func cancel() { timer?.invalidate(); timer = nil }
    func move(to frame: NSRect, animated: Bool) {
        cancel()
        guard let window = window else { return }
        target = frame
        guard animated, window.isVisible, !NSWorkspace.shared.accessibilityDisplayShouldReduceMotion else {
            window.setFrame(frame, display: true); return
        }
        start = window.frame
        guard start != frame else { return }
        started = CACurrentMediaTime()
        let timer = Timer(timeInterval: 1 / 120, repeats: true) { [weak self] _ in self?.tick() }
        self.timer = timer
        RunLoop.main.add(timer, forMode: .common)
    }
    static func progress(_ time: Double) -> Double {
        if time <= 0 { return 0 }
        if time >= 1 { return 1 }
        // Smooth ease-in-out: gradual start and finish, with no overshoot.
        if time < 0.5 { return 4 * time * time * time }
        return 1 - pow(-2 * time + 2, 3) / 2
    }
    func tick() {
        guard let window = window else { cancel(); return }
        let elapsed = (CACurrentMediaTime() - started) / duration
        if elapsed >= 1 { cancel(); window.setFrame(target, display: true); return }
        let p = CGFloat(Self.progress(elapsed))
        let width = max(1, start.width + (target.width - start.width) * p)
        let height = max(1, start.height + (target.height - start.height) * p)
        // Interpolate the anchor, keeping the search header stationary while
        // the list unfolds downwards. Retarget from the current frame on input.
        let center = start.midX + (target.midX - start.midX) * p
        let top = start.maxY + (target.maxY - start.maxY) * p
        window.setFrame(NSRect(x: center - width / 2, y: top - height, width: width, height: height), display: true)
    }
    deinit { cancel() }
}
