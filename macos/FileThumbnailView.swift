import AppKit
import QuickLookThumbnailing

/// A centered thumbnail for compact inspectors; full live previews remain separate.
final class CenteredFilePreviewView: NSView {
    let imageView = NSImageView(frame: .zero)

    private var currentRequest: QLThumbnailGenerator.Request?
    private var showingFallback = true
    private let previewInset: CGFloat = 12

    init(frame: NSRect, url: URL, fallback: NSImage) {
        super.init(frame: frame)
        imageView.imageAlignment = .alignCenter
        imageView.imageScaling = .scaleProportionallyDown
        imageView.isEditable = false
        addSubview(imageView)
        configure(url: url, fallback: fallback)
    }

    required init?(coder: NSCoder) {
        return nil
    }

    deinit {
        if let request = currentRequest {
            QLThumbnailGenerator.shared.cancel(request)
        }
    }

    /// Replace the source, keeping a small file icon visible until generation succeeds.
    func configure(url: URL, fallback: NSImage) {
        precondition(Thread.isMainThread)
        if let request = currentRequest {
            QLThumbnailGenerator.shared.cancel(request)
        }
        showingFallback = true
        imageView.image = fallback
        layoutImageView()

        let request = QLThumbnailGenerator.Request(
            fileAt: url,
            size: CGSize(width: max(1, bounds.width - previewInset * 2),
                         height: max(1, bounds.height - previewInset * 2)),
            scale: window?.backingScaleFactor ?? NSScreen.main?.backingScaleFactor ?? 2,
            representationTypes: .thumbnail
        )
        request.iconMode = false
        currentRequest = request

        QLThumbnailGenerator.shared.generateBestRepresentation(for: request) { [weak self] thumbnail, _ in
            DispatchQueue.main.async { [weak self] in
                guard let self = self, self.currentRequest === request else { return }
                self.currentRequest = nil
                guard let thumbnail = thumbnail else { return }
                self.showingFallback = false
                self.imageView.image = thumbnail.nsImage
                self.layoutImageView()
            }
        }
    }

    override func layout() {
        super.layout()
        layoutImageView()
    }

    private func layoutImageView() {
        let available = NSRect(
            x: bounds.minX + min(previewInset, bounds.width / 2),
            y: bounds.minY + min(previewInset, bounds.height / 2),
            width: max(0, bounds.width - previewInset * 2),
            height: max(0, bounds.height - previewInset * 2)
        )
        if showingFallback {
            let width = min(96, available.width)
            let height = min(96, available.height)
            imageView.frame = NSRect(x: bounds.midX - width / 2,
                                     y: bounds.midY - height / 2,
                                     width: width, height: height)
        } else {
            imageView.frame = available
        }
    }

    override var isOpaque: Bool { true }

    override func draw(_ dirtyRect: NSRect) {
        NSColor.controlBackgroundColor.setFill()
        bounds.fill()
    }
}
