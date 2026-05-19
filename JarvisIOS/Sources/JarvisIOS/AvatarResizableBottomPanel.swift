import SwiftUI

/// Bottom sheet over the Bevy viewport: drag handle resizes height; content scrolls inside.
struct AvatarResizableBottomPanel<Header: View, Content: View>: View {
    @Binding var height: CGFloat
    let minHeight: CGFloat
    let maxHeight: CGFloat
  @ViewBuilder let header: () -> Header
    @ViewBuilder let content: () -> Content

    @GestureState private var dragDelta: CGFloat = 0

    var body: some View {
        VStack(spacing: 0) {
            Capsule()
                .fill(Color.secondary.opacity(0.45))
                .frame(width: 40, height: 5)
                .padding(.top, 8)
                .padding(.bottom, 6)
                .contentShape(Rectangle())
                .gesture(
                    DragGesture(minimumDistance: 4)
                        .updating($dragDelta) { value, state, _ in
                            state = -value.translation.height
                        }
                        .onEnded { value in
                            let proposed = height - value.translation.height
                            height = min(max(proposed, minHeight), maxHeight)
                        }
                )

            header()
                .padding(.horizontal, 12)
                .padding(.bottom, 6)

            Divider()

            content()
                .frame(maxWidth: .infinity, maxHeight: .infinity)
        }
        .frame(height: min(max(height + dragDelta, minHeight), maxHeight))
        .frame(maxWidth: .infinity)
        .background(.ultraThinMaterial)
        .clipShape(RoundedRectangle(cornerRadius: 16, style: .continuous))
        .overlay(
            RoundedRectangle(cornerRadius: 16, style: .continuous)
                .strokeBorder(Color.primary.opacity(0.12), lineWidth: 1)
        )
        .shadow(color: .black.opacity(0.2), radius: 12, y: 4)
    }
}
