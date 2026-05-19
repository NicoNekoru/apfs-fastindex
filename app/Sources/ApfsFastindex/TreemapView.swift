import AppKit
import CApfsFastindex
import SwiftUI

/// Delegate the `TreemapView` posts user interactions back to.
/// Phase 4 only emits click; phase 4b will add context-menu and
/// hover-changed for the tooltip.
protocol TreemapViewDelegate: AnyObject {
    func treemapView(_ view: TreemapView, didClickCell nodeIndex: UInt32)
}

/// NSView subclass that paints an `Scan.Layout` via Core
/// Graphics. Consumes a pre-laid-out `UnsafeBufferPointer<ApfsCell>`
/// directly from Rust (no copy), so the per-frame draw cost is
/// bounded by CG fill-rect throughput.
///
/// Drawing order matches the canvas-era three-pass pipeline:
///   1. Background fill (one `fillRect`).
///   2. Dir backgrounds — all share one fillStyle so we set the
///      colour once and walk the cells.
///   3. Leaves grouped by `fill_rgb` — collect indices into a
///      dictionary by colour, then walk each group with one
///      fillStyle. Cuts per-cell state changes from O(cells) to
///      O(unique-colours), which is in the low tens.
///   4. Hover outline.
final class TreemapView: NSView {
    /// Set whenever the layout changes (new scan, new
    /// depth/metric, navigation). Triggers a redraw via
    /// `needsDisplay = true`. Holding a strong reference here
    /// keeps the Rust `ApfsLayout` alive for the lifetime of
    /// this view's display.
    var layout: Scan.Layout? {
        didSet {
            hoveredIndex = nil
            needsDisplay = true
        }
    }

    /// Cell currently under the cursor (or nil). Used to draw the
    /// hover outline overlay; doesn't trigger any FFI back into
    /// Rust on every paint.
    private var hoveredIndex: UInt32? {
        didSet {
            if hoveredIndex != oldValue {
                needsDisplay = true
            }
        }
    }

    weak var delegate: TreemapViewDelegate?

    /// Coordinate origin at the top-left matches the cell coords
    /// Rust hands us (and the canvas-era code we ported from).
    /// Without this AppKit draws bottom-up; cells would flip
    /// across the y-axis.
    override var isFlipped: Bool { true }

    // MARK: - Drawing

    override func draw(_ dirtyRect: NSRect) {
        guard let ctx = NSGraphicsContext.current?.cgContext else { return }

        // Phase 1: bg fill. Matches `VizPalette.bg` (#0f1115).
        ctx.setFillColor(red: 0x0f / 255.0, green: 0x11 / 255.0, blue: 0x15 / 255.0, alpha: 1.0)
        ctx.fill(bounds)

        guard let layout, layout.count > 0 else { return }
        let cells = layout.cells

        // Phase 2: dir backgrounds. One fillStyle for all of them
        // (matches the canvas renderer's
        // `rgba(30, 35, 45, 0.55)` — same shade post-blend on a
        // 0x0f1115 backdrop).
        ctx.setFillColor(
            red: 30.0 / 255.0,
            green: 35.0 / 255.0,
            blue: 45.0 / 255.0,
            alpha: 0.55
        )
        for c in cells {
            if c.flags & TreemapView.flagDir == 0 { continue }
            ctx.fill(CGRect(
                x: CGFloat(c.x0), y: CGFloat(c.y0),
                width: CGFloat(c.x1 - c.x0),
                height: CGFloat(c.y1 - c.y0)
            ))
        }

        // Phase 3: leaves grouped by `fill_rgb`. Build the
        // per-colour buckets in one pass over the cell array,
        // then walk each bucket with one `setFillColor` call.
        // `UnsafeBufferPointer` iteration is just a pointer-bump
        // loop — no Swift array allocation in the hot path.
        var groups: [UInt32: [Int]] = [:]
        for i in 0..<cells.count {
            let c = cells[i]
            if c.flags & TreemapView.flagDir != 0 { continue }
            groups[c.fill_rgb, default: []].append(i)
        }
        for (rgb, indices) in groups {
            let r = CGFloat((rgb >> 16) & 0xff) / 255.0
            let g = CGFloat((rgb >> 8) & 0xff) / 255.0
            let b = CGFloat(rgb & 0xff) / 255.0
            ctx.setFillColor(red: r, green: g, blue: b, alpha: 1.0)
            for idx in indices {
                let c = cells[idx]
                ctx.fill(CGRect(
                    x: CGFloat(c.x0), y: CGFloat(c.y0),
                    width: CGFloat(c.x1 - c.x0),
                    height: CGFloat(c.y1 - c.y0)
                ))
            }
        }

        // Phase 4: hover overlay. Single stroked rect on top.
        if let hovered = hoveredIndex, Int(hovered) < cells.count {
            let c = cells[Int(hovered)]
            ctx.setStrokeColor(red: 1.0, green: 1.0, blue: 1.0, alpha: 0.55)
            ctx.setLineWidth(1.5)
            // Inset by 0.75 (half the stroke width) so the
            // outline sits on top of the cell's fill instead
            // of straddling its edge.
            ctx.stroke(CGRect(
                x: CGFloat(c.x0) + 0.75, y: CGFloat(c.y0) + 0.75,
                width: CGFloat(c.x1 - c.x0) - 1.5,
                height: CGFloat(c.y1 - c.y0) - 1.5
            ))
        }
    }

    // MARK: - Tracking + hit-test

    private var trackingArea: NSTrackingArea?

    override func updateTrackingAreas() {
        super.updateTrackingAreas()
        if let existing = trackingArea {
            removeTrackingArea(existing)
        }
        // `.activeInActiveApp` + `.mouseMoved` is enough for the
        // hover loop; `.inVisibleRect` lets the tracking area
        // resize with the view automatically.
        let area = NSTrackingArea(
            rect: .zero,
            options: [.activeInActiveApp, .mouseMoved, .mouseEnteredAndExited, .inVisibleRect],
            owner: self,
            userInfo: nil
        )
        addTrackingArea(area)
        trackingArea = area
    }

    override func mouseMoved(with event: NSEvent) {
        updateHover(for: event)
    }

    override func mouseEntered(with event: NSEvent) {
        updateHover(for: event)
    }

    override func mouseExited(with event: NSEvent) {
        hoveredIndex = nil
    }

    override func mouseDown(with event: NSEvent) {
        guard let layout else { return }
        let p = convert(event.locationInWindow, from: nil)
        guard let hit = layout.hitTest(x: Float(p.x), y: Float(p.y)) else { return }
        let cell = layout.cells[Int(hit)]
        delegate?.treemapView(self, didClickCell: cell.node_index)
    }

    private func updateHover(for event: NSEvent) {
        guard let layout else {
            hoveredIndex = nil
            return
        }
        let p = convert(event.locationInWindow, from: nil)
        let hit = layout.hitTest(x: Float(p.x), y: Float(p.y))
        hoveredIndex = hit
    }

    // MARK: - Cell flag constants
    // Mirror the `CELL_FLAG_*` bits in `render.rs`. The numerical
    // values are intentionally hardcoded — the FFI doesn't
    // expose them as Swift symbols today; if `render.rs` adds
    // more bits, update here.
    static let flagDir: UInt32 = 1 << 0
    static let flagSymlink: UInt32 = 1 << 1
    static let flagPaddingTop: UInt32 = 1 << 2
}

/// SwiftUI wrapper around the `TreemapView`. `layout` and the
/// click handler are both bindings the surrounding View
/// supplies — the wrapper is the bridge between the SwiftUI
/// data model (in phase 5: `NativeScanController`) and the
/// AppKit drawing surface.
struct TreemapViewRepresentable: NSViewRepresentable {
    let layout: Scan.Layout?
    let onClick: (UInt32) -> Void

    func makeNSView(context: Context) -> TreemapView {
        let view = TreemapView()
        view.delegate = context.coordinator
        return view
    }

    func updateNSView(_ nsView: TreemapView, context: Context) {
        // Identity check: only reseat when the Layout reference
        // actually changes. `Layout` is a class, so `===` is the
        // right comparator — re-setting the same layout would
        // trigger a hover-state reset (see `didSet` on `layout`)
        // for no reason.
        if nsView.layout !== layout {
            nsView.layout = layout
        }
        context.coordinator.onClick = onClick
    }

    func makeCoordinator() -> Coordinator {
        Coordinator(onClick: onClick)
    }

    final class Coordinator: NSObject, TreemapViewDelegate {
        var onClick: (UInt32) -> Void
        init(onClick: @escaping (UInt32) -> Void) {
            self.onClick = onClick
        }
        func treemapView(_ view: TreemapView, didClickCell nodeIndex: UInt32) {
            onClick(nodeIndex)
        }
    }
}
