import SwiftUI

/// Bottom sheet over the Bevy viewport: drag handle resizes height; content scrolls inside.
///
/// Uses an opaque panel background (not live blur) and pixel-snapped heights to avoid jitter while
/// scrolling or dragging the resize handle.
struct AvatarResizableBottomPanel<Header: View, Content: View>: View {
    @Binding var height: CGFloat
    let minHeight: CGFloat
    let maxHeight: CGFloat
    @ViewBuilder let header: () -> Header
    @ViewBuilder let content: () -> Content

    @State private var isDragging = false
    @State private var dragHeight: CGFloat = 0
    @State private var heightAtDragStart: CGFloat = 0

    private var displayHeight: CGFloat {
        let raw = isDragging ? dragHeight : height
        return snap(min(max(raw, minHeight), maxHeight))
    }

    var body: some View {
        VStack(spacing: 0) {
            resizeHandle

            header()
                .padding(.horizontal, 12)
                .padding(.bottom, 6)

            Divider()

            content()
                .frame(maxWidth: .infinity, maxHeight: .infinity)
                .clipped()
        }
        .frame(height: displayHeight)
        .frame(maxWidth: .infinity)
        .background(panelBackground)
        .clipShape(RoundedRectangle(cornerRadius: 16, style: .continuous))
        .overlay(
            RoundedRectangle(cornerRadius: 16, style: .continuous)
                .strokeBorder(Color.primary.opacity(0.12), lineWidth: 1)
        )
        .shadow(color: .black.opacity(0.18), radius: 8, y: 3)
        .animation(nil, value: displayHeight)
        .onAppear {
            dragHeight = snap(height)
        }
        .onChange(of: height) { _, newValue in
            if !isDragging {
                dragHeight = snap(newValue)
            }
        }
    }

    private var resizeHandle: some View {
        Capsule()
            .fill(Color.secondary.opacity(0.45))
            .frame(width: 40, height: 5)
            .padding(.top, 8)
            .padding(.bottom, 6)
            .frame(maxWidth: .infinity)
            .contentShape(Rectangle().size(width: 200, height: 28))
            .highPriorityGesture(
                DragGesture(minimumDistance: 2)
                    .onChanged { value in
                        if !isDragging {
                            isDragging = true
                            heightAtDragStart = height
                        }
                        let proposed = heightAtDragStart - value.translation.height
                        dragHeight = snap(min(max(proposed, minHeight), maxHeight))
                    }
                    .onEnded { value in
                        let proposed = heightAtDragStart - value.translation.height
                        height = snap(min(max(proposed, minHeight), maxHeight))
                        dragHeight = height
                        isDragging = false
                    }
            )
    }

    private var panelBackground: some View {
        RoundedRectangle(cornerRadius: 16, style: .continuous)
            .fill(Color(uiColor: .systemBackground).opacity(0.96))
    }

    private func snap(_ value: CGFloat) -> CGFloat {
        let scale = max(UIScreen.main.scale, 1)
        return (value * scale).rounded() / scale
    }
}
