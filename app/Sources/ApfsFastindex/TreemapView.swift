import AppKit
import ApfsCore
import CApfsFastindex
import SwiftUI

/// Delegate the `TreemapView` posts user interactions back to.
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
            updateSelectionRect()
            invalidateStaticCache()
            needsDisplay = true
        }
    }

    /// The `Scan` the layout was produced from. Used by the
    /// label pass to look up node names and per-cell values
    /// for the "name · size" dir label / "name" leaf label.
    /// Held weakly conceptually — the same `Scan` ARC chain
    /// also keeps the `Layout` alive, so by the time the view
    /// would dereference a freed scan the layout is gone too.
    var scan: Scan? {
        didSet {
            invalidateStaticCache()
            needsDisplay = true
        }
    }

    /// Active size metric. Drives whether the dir-label suffix
    /// renders the logical or allocated total.
    var metric: Scan.Metric = .logical {
        didSet {
            invalidateStaticCache()
            needsDisplay = true
        }
    }

    /// Cached render of the "static" treemap content — every
    /// pass that doesn't depend on the hover state: background
    /// fill, dir backgrounds, dir strokes, leaf fills, labels.
    /// On a `/`-scan that draw runs over hundreds of thousands
    /// of cells plus a per-label NSAttributedString round-trip;
    /// re-running it for every mouse-move tick is what made
    /// the hover laggy on full-screen treemaps.
    ///
    /// We render the static content into an `NSImage` once per
    /// layout change (or window resize / metric flip), then
    /// blit the image into the dirty rect and draw only the
    /// hover overlay + tooltip on top per paint. Mouse-moves
    /// become essentially free: one image draw + one rect
    /// stroke + a tiny text draw.
    private var cachedStatic: NSImage?
    private var cachedStaticBoundsSize: CGSize = .zero

    private func invalidateStaticCache() {
        cachedStatic = nil
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

    /// Last cursor position in view-local (flipped) coords.
    /// Drives the tooltip's anchor on the next paint.
    private var hoverPoint: CGPoint = .zero

    weak var fileActions: FileActions?
    var selectedNode: UInt32 = 0 {
        didSet {
            guard selectedNode != oldValue else { return }
            updateSelectionRect()
            needsDisplay = true
        }
    }
    var highlightedNodes: Set<UInt32>? {
        didSet {
            guard highlightedNodes != oldValue else { return }
            invalidateStaticCache()
            needsDisplay = true
        }
    }
    private var selectionRect: CGRect?

    private func updateSelectionRect() {
        selectionRect = layout?.cells.first(where: { $0.node_index == selectedNode }).map {
            CGRect(x: CGFloat($0.x0), y: CGFloat($0.y0), width: CGFloat($0.x1 - $0.x0), height: CGFloat($0.y1 - $0.y0))
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

        // Re-render the static cache on first paint, after a
        // layout/scan/metric change, or on a window resize.
        if cachedStatic == nil || cachedStaticBoundsSize != bounds.size {
            rebuildStaticCache()
        }

        // Blit the cached static treemap. `compositingOperation`
        // = `.copy` overwrites any previous frame pixels — the
        // image is opaque (bg fill in phase 1) so this is
        // equivalent to filling+drawing in one step.
        //
        // `respectFlipped: true` is critical: the image was
        // rendered with `lockFocusFlipped(true)` (its internal
        // representation has the y-flip baked in) AND the
        // destination (this view) has `isFlipped = true`. The
        // 4-arg `draw(in:from:operation:fraction:)` overload
        // doesn't honour the image's flip state, so the two
        // cancellations don't line up — the result is upside-
        // down. The 6-arg overload with `respectFlipped: true`
        // correctly cancels the image's flip against the
        // destination's flip and renders right-side-up.
        if let img = cachedStatic {
            img.draw(
                in: bounds,
                from: NSRect(origin: .zero, size: bounds.size),
                operation: .copy,
                fraction: 1.0,
                respectFlipped: true,
                hints: nil
            )
        } else {
            // No layout yet — paint the bg colour so the view
            // isn't black/transparent.
            ctx.setFillColor(red: 0x0f / 255.0, green: 0x11 / 255.0, blue: 0x15 / 255.0, alpha: 1.0)
            ctx.fill(bounds)
        }

        if let rect = selectionRect, selectedNode != 0, rect.width >= 3, rect.height >= 3 {
            ctx.setStrokeColor(NSColor.controlAccentColor.cgColor)
            ctx.setLineWidth(2)
            ctx.stroke(rect.insetBy(dx: 1, dy: 1))
        }

        // Hover overlay + tooltip. Drawn fresh per paint over
        // the cached static layer — that's the only part of the
        // canvas that changes between mouse-move events.
        if let layout, let hovered = hoveredIndex,
           Int(hovered) < layout.count, let scan
        {
            let c = layout.cells[Int(hovered)]
            ctx.setStrokeColor(red: 1.0, green: 1.0, blue: 1.0, alpha: 0.55)
            ctx.setLineWidth(1.5)
            ctx.stroke(CGRect(
                x: CGFloat(c.x0) + 0.75, y: CGFloat(c.y0) + 0.75,
                width: CGFloat(c.x1 - c.x0) - 1.5,
                height: CGFloat(c.y1 - c.y0) - 1.5
            ))
            drawTooltip(ctx: ctx, cell: c, scan: scan)
        }
    }

    /// Render every layer that doesn't depend on hover state
    /// (bg, dir fills + strokes, leaves, labels) into an
    /// offscreen `NSImage`. Called once per layout/scan/metric
    /// change or window resize; mouse-move ticks reuse the
    /// cached image.
    private func rebuildStaticCache() {
        let size = bounds.size
        guard size.width > 0, size.height > 0 else {
            cachedStatic = nil
            cachedStaticBoundsSize = .zero
            return
        }

        let img = NSImage(size: size)
        // `lockFocusFlipped(true)` makes the offscreen context
        // y-flipped to match our view's `isFlipped = true`, so
        // the same cell coords from Rust draw the same shape
        // here as they would on-screen.
        img.lockFocusFlipped(true)
        defer { img.unlockFocus() }

        guard let ctx = NSGraphicsContext.current?.cgContext else {
            cachedStatic = nil
            cachedStaticBoundsSize = .zero
            return
        }

        // Phase 1: bg fill. Matches `VizPalette.bg` (#0f1115).
        ctx.setFillColor(red: 0x0f / 255.0, green: 0x11 / 255.0, blue: 0x15 / 255.0, alpha: 1.0)
        ctx.fill(CGRect(origin: .zero, size: size))

        guard let layout, layout.count > 0 else {
            cachedStatic = img
            cachedStaticBoundsSize = size
            return
        }
        let cells = layout.cells

        // Phase 2: dir backgrounds. One fillStyle for all of them
        // (matches the canvas renderer's
        // `rgba(30, 35, 45, 0.55)` — same shade post-blend on a
        // 0x0f1115 backdrop). Then a second pass strokes a thin
        // border around each dir so containers are visually
        // distinct from their leaves on top — the WizTree look
        // the JS renderer used to do via canvas `strokeRect`.
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
        // Stroke pass — a slightly lighter outline on every dir.
        // Done as its own loop so the fill colour set above
        // doesn't have to be re-set per cell.
        ctx.setStrokeColor(
            red: 0x4a / 255.0,
            green: 0x52 / 255.0,
            blue: 0x60 / 255.0,
            alpha: 0.55
        )
        ctx.setLineWidth(1.0)
        for c in cells {
            if c.flags & TreemapView.flagDir == 0 { continue }
            let w = CGFloat(c.x1 - c.x0)
            let h = CGFloat(c.y1 - c.y0)
            // Skip the stroke pass on tiny dirs — at /-scale most
            // dirs are < 4 pt and the stroke would just smear.
            if w < 4 || h < 4 { continue }
            // Inset by half the stroke width so the line sits
            // inside the dir rect rather than straddling its edge
            // (which would clip into the parent dir's fill).
            ctx.stroke(CGRect(
                x: CGFloat(c.x0) + 0.5, y: CGFloat(c.y0) + 0.5,
                width: w - 1, height: h - 1
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

        // Phase 5: labels. Pre-compute style + paragraph once
        // and re-use across every label draw — NSAttributedString
        // allocation per cell is the real per-frame cost.
        if let scan {
            drawLabels(cells: cells, scan: scan)
        }

        if let highlightedNodes {
            ctx.setFillColor(red: 0.06, green: 0.07, blue: 0.08, alpha: 0.78)
            for cell in cells where cell.flags & Self.flagDir == 0 && !highlightedNodes.contains(cell.node_index) {
                ctx.fill(CGRect(x: CGFloat(cell.x0), y: CGFloat(cell.y0),
                                width: CGFloat(cell.x1 - cell.x0), height: CGFloat(cell.y1 - cell.y0)))
            }
        }
        cachedStatic = img
        cachedStaticBoundsSize = size
    }

    // MARK: - Label drawing

    /// Per-cell labels. Dirs render `"name · size"` in the
    /// 11 pt-semibold dir-label colour; leaves render `"name"`
    /// in 10 pt regular. Cells too small to host a label
    /// (`< MIN_DIR_LABEL_W` or `< MIN_LEAF_LABEL_W`) are skipped
    /// — at /-scale most cells are tiny so the loop is
    /// bounded by the ~hundreds of large cells, not by the
    /// total cell count.
    ///
    /// Uses `NSString.draw(in:withAttributes:)` with
    /// `lineBreakMode = .byTruncatingTail` so AppKit handles
    /// the trailing-ellipsis truncation natively. CTLine would
    /// be ~2× faster but adds the manual y-flip dance; the
    /// NSString path stays within budget at typical view
    /// sizes.
    private func drawLabels(cells: UnsafeBufferPointer<ApfsCell>, scan: Scan) {
        let dirFont = AppFont.ns(11, bold: true)
        let leafFont = AppFont.ns(10)
        let dirColor = NSColor(
            red: 0xcf / 255.0, green: 0xd6 / 255.0,
            blue: 0xe4 / 255.0, alpha: 1.0
        )
        let leafColor = NSColor(
            red: 0x0b / 255.0, green: 0x0d / 255.0,
            blue: 0x12 / 255.0, alpha: 1.0
        )
        let paragraph = NSMutableParagraphStyle()
        paragraph.lineBreakMode = .byTruncatingTail
        let dirAttrs: [NSAttributedString.Key: Any] = [
            .font: dirFont,
            .foregroundColor: dirColor,
            .paragraphStyle: paragraph,
        ]
        let leafAttrs: [NSAttributedString.Key: Any] = [
            .font: leafFont,
            .foregroundColor: leafColor,
            .paragraphStyle: paragraph,
        ]
        let byteFormatter = ByteCountFormatter()
        byteFormatter.countStyle = .binary
        byteFormatter.allowedUnits = [.useGB, .useMB, .useKB, .useBytes]
        byteFormatter.zeroPadsFractionDigits = false

        for c in cells {
            let w = c.x1 - c.x0
            let h = c.y1 - c.y0
            let isDir = c.flags & TreemapView.flagDir != 0
            let minW: Float = isDir ? Float(TreemapView.minDirLabelW) : Float(TreemapView.minLeafLabelW)
            let minH: Float = isDir ? Float(TreemapView.minDirLabelH) : Float(TreemapView.minLeafLabelH)
            if w < minW || h < minH { continue }
            guard let rawName = scan.name(of: c.node_index), !rawName.isEmpty else { continue }
            let name = DisplaySanitizer.sanitiseDisplay(rawName)
            let label: String
            if isDir {
                let value = metric == .allocated
                    ? (scan.valueAllocated(of: c.node_index) ?? 0)
                    : scan.valueLogical(of: c.node_index)
                label = "\(name) · \(byteFormatter.string(fromByteCount: Int64(value)))"
            } else {
                label = name
            }
            let rect = NSRect(
                x: CGFloat(c.x0) + 4,
                y: CGFloat(c.y0) + (isDir ? 1 : 2),
                width: CGFloat(w) - 8,
                height: CGFloat(isDir ? 14 : 12)
            )
            label.draw(in: rect, withAttributes: isDir ? dirAttrs : leafAttrs)
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
        if event.modifierFlags.contains(.control) { super.mouseDown(with: event); return }
        guard let layout else { return }
        let p = convert(event.locationInWindow, from: nil)
        guard let hit = layout.hitTest(x: Float(p.x), y: Float(p.y)) else { return }
        let cell = layout.cells[Int(hit)]
        delegate?.treemapView(self, didClickCell: cell.node_index)
    }

    override func menu(for event: NSEvent) -> NSMenu? {
        guard let layout, let scan, let fileActions else { return nil }
        let point = convert(event.locationInWindow, from: nil)
        guard let hit = layout.hitTest(x: Float(point.x), y: Float(point.y)),
              let target = fileActions.target(node: layout.cells[Int(hit)].node_index, scan: scan)
        else { return nil }
        return fileActions.menu(for: target)
    }

    private func updateHover(for event: NSEvent) {
        guard let layout else {
            hoveredIndex = nil
            return
        }
        let p = convert(event.locationInWindow, from: nil)
        hoverPoint = p
        let hit = layout.hitTest(x: Float(p.x), y: Float(p.y))
        if hoveredIndex != hit {
            hoveredIndex = hit
        } else if hit != nil {
            // Same cell, but the cursor moved — the tooltip
            // follows the pointer so we still want a redraw.
            needsDisplay = true
        }
    }

    /// Floating tooltip drawn near the cursor for the currently
    /// hovered cell. Two-line layout:
    ///   "name · size"     (12 pt semibold)
    ///   "absolute path"   (10 pt regular, truncated tail)
    /// Anchored to `hoverPoint` with an offset so the tooltip
    /// doesn't sit under the cursor; flips to the other side of
    /// the cursor when it would clip the right/bottom edge.
    private func drawTooltip(ctx: CGContext, cell: ApfsCell, scan: Scan) {
        // Sanitise both name and path before drawing — parser
        // bytes drawn directly into the tooltip would let a
        // crafted volume splice RTL overrides or NULs into
        // visible text (audit #App-2 spoofing class). The
        // sanitiser replaces offenders with U+FFFD so the user
        // sees that the name carried something unprintable.
        let rawName = (cell.node_index == 0 ? "/" : (scan.name(of: cell.node_index) ?? "?"))
        let rawPath = (cell.node_index == 0 ? "/" : (scan.path(of: cell.node_index) ?? ""))
        let name = DisplaySanitizer.sanitiseDisplay(rawName)
        let path = DisplaySanitizer.sanitiseDisplay(rawPath)
        let byteFormatter = ByteCountFormatter()
        byteFormatter.countStyle = .binary
        byteFormatter.allowedUnits = [.useGB, .useMB, .useKB, .useBytes]
        let value: UInt64 = metric == .allocated
            ? (scan.valueAllocated(of: cell.node_index) ?? 0)
            : scan.valueLogical(of: cell.node_index)
        let sizeText = byteFormatter.string(fromByteCount: Int64(value))
        let titleText = "\(name) · \(sizeText)"

        let titleFont = AppFont.ns(12, bold: true)
        let pathFont = AppFont.ns(10)
        let titleColor = NSColor(white: 0.92, alpha: 1.0)
        let pathColor = NSColor(white: 0.60, alpha: 1.0)
        let para = NSMutableParagraphStyle()
        para.lineBreakMode = .byTruncatingHead
        let titleAttrs: [NSAttributedString.Key: Any] = [
            .font: titleFont,
            .foregroundColor: titleColor,
        ]
        let pathAttrs: [NSAttributedString.Key: Any] = [
            .font: pathFont,
            .foregroundColor: pathColor,
            .paragraphStyle: para,
        ]
        let title = NSAttributedString(string: titleText, attributes: titleAttrs)
        let pathLine = NSAttributedString(string: path, attributes: pathAttrs)

        let titleSize = title.size()
        // Cap the path-line width so a deep `/Users/…/file.txt`
        // doesn't stretch the tooltip across the window. AppKit
        // truncates with the paragraph style above.
        let maxPathW: CGFloat = 380
        let pathW = min(pathLine.size().width, maxPathW)
        let pad: CGFloat = 8
        let lineGap: CGFloat = 2
        let cardW = max(titleSize.width, pathW) + pad * 2
        let cardH = titleSize.height + lineGap + pathLine.size().height + pad * 2

        // Anchor below-right of the cursor; flip to the opposite
        // side if it would overflow `bounds`.
        var x = hoverPoint.x + 14
        var y = hoverPoint.y + 14
        if x + cardW > bounds.width - 4 { x = hoverPoint.x - cardW - 14 }
        if y + cardH > bounds.height - 4 { y = hoverPoint.y - cardH - 14 }
        x = max(4, x)
        y = max(4, y)

        let card = CGRect(x: x, y: y, width: cardW, height: cardH)
        ctx.setFillColor(red: 0x12 / 255, green: 0x14 / 255, blue: 0x1b / 255, alpha: 0.96)
        ctx.fill(card)
        ctx.setStrokeColor(red: 0x4a / 255, green: 0x52 / 255, blue: 0x60 / 255, alpha: 0.9)
        ctx.setLineWidth(1.0)
        ctx.stroke(card.insetBy(dx: 0.5, dy: 0.5))

        title.draw(at: CGPoint(x: x + pad, y: y + pad))
        let pathRect = CGRect(
            x: x + pad,
            y: y + pad + titleSize.height + lineGap,
            width: cardW - pad * 2,
            height: pathLine.size().height
        )
        pathLine.draw(in: pathRect)
    }

    // MARK: - Cell flag constants
    // Mirror the `CELL_FLAG_*` bits in `render.rs`. The numerical
    // values are intentionally hardcoded — the FFI doesn't
    // expose them as Swift symbols today; if `render.rs` adds
    // more bits, update here.
    static let flagDir: UInt32 = 1 << 0
    static let flagSymlink: UInt32 = 1 << 1
    static let flagPaddingTop: UInt32 = 1 << 2

    // Minimum cell dimensions to host a label. Mirror the
    // `MIN_*_LABEL_*` constants from `render.rs` so the
    // Rust-side `paddingTop` heuristic and the Swift-side
    // "should I bother drawing text here" check agree.
    static let minDirLabelW: CGFloat = 48
    static let minDirLabelH: CGFloat = 16
    static let minLeafLabelW: CGFloat = 40
    static let minLeafLabelH: CGFloat = 14
}

/// SwiftUI wrapper around the `TreemapView`. `layout`, `scan`,
/// and `metric` all flow in from the surrounding View; the
/// wrapper threads them onto the NSView so the label-drawing
/// pass can look up names and per-node values without a
/// separate trip through SwiftUI state.
struct TreemapViewRepresentable: NSViewRepresentable {
    let scan: Scan?
    let layout: Scan.Layout?
    let metric: Scan.Metric
    let fileActions: FileActions
    let selectedNode: UInt32
    let highlightedNodes: Set<UInt32>?
    let onClick: (UInt32) -> Void

    func makeNSView(context: Context) -> TreemapView {
        let view = TreemapView()
        view.delegate = context.coordinator
        return view
    }

    func updateNSView(_ nsView: TreemapView, context: Context) {
        // Identity check on the layout to avoid clobbering hover
        // state when the same Layout reference is handed back to
        // us during an unrelated re-render.
        if nsView.layout !== layout {
            nsView.layout = layout
        }
        if nsView.scan !== scan {
            nsView.scan = scan
        }
        if nsView.metric != metric {
            nsView.metric = metric
        }
        nsView.fileActions = fileActions
        nsView.selectedNode = selectedNode
        nsView.highlightedNodes = highlightedNodes
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
