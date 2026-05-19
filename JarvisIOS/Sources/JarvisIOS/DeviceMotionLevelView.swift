import SwiftUI

/// Bubble-level visualization for phone tilt + shake (CoreMotion → spring gravity).
struct DeviceMotionLevelView: View {
    @Bindable var motion: JarvisDeviceMotion
    @State private var showTuning = false

    private let levelSize: CGFloat = 132

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack(alignment: .top, spacing: 12) {
                levelCircle
                readoutColumn
            }

            DisclosureGroup("Motion tuning", isExpanded: $showTuning) {
                tuningSliders
                    .padding(.top, 6)
            }
            .font(.subheadline)
        }
        .padding(.vertical, 4)
        .onAppear {
            JarvisBevySession.pushDeviceMotionTuning()
        }
    }

    private var levelCircle: some View {
        ZStack {
            Circle()
                .strokeBorder(Color.secondary.opacity(0.35), lineWidth: 2)
            Circle()
                .strokeBorder(Color.accentColor.opacity(0.25), lineWidth: 1)
                .padding(10)
            Circle()
                .fill(Color.primary.opacity(0.06))
            crosshair
            tiltDot
            if motion.shakeMagnitude > 0.05 {
                Circle()
                    .stroke(Color.orange.opacity(shakeRingOpacity), lineWidth: 3)
                    .padding(shakeRingInset)
            }
        }
        .frame(width: levelSize, height: levelSize)
        .accessibilityLabel("Phone tilt level")
        .accessibilityValue("\(Int(motion.tiltDegrees.rounded())) degrees tilt")
    }

    private var crosshair: some View {
        ZStack {
            Rectangle()
                .fill(Color.secondary.opacity(0.25))
                .frame(width: 1, height: levelSize * 0.55)
            Rectangle()
                .fill(Color.secondary.opacity(0.25))
                .frame(width: levelSize * 0.55, height: 1)
        }
    }

    /// Dot position from horizontal gravity components (Bevy X/Z).
    private var tiltDot: some View {
        let g = motion.gravityDisplay
        let hx = g.x
        let hz = g.z
        let horiz = sqrt(hx * hx + hz * hz)
        let maxR = levelSize * 0.38
        let scale = horiz > 1e-5 ? min(1.0, horiz) * maxR / horiz : 0
        let dx = hx * scale
        let dy = -hz * scale
        return Circle()
            .fill(motion.enabled ? Color.accentColor : Color.secondary)
            .frame(width: 14, height: 14)
            .shadow(color: .black.opacity(0.2), radius: 2, y: 1)
            .offset(x: dx, y: dy)
            .animation(.interactiveSpring(response: 0.22, dampingFraction: 0.82), value: dx)
            .animation(.interactiveSpring(response: 0.22, dampingFraction: 0.82), value: dy)
    }

    private var shakeRingOpacity: Double {
        min(0.85, 0.25 + motion.shakeMagnitude * 0.35)
    }

    private var shakeRingInset: CGFloat {
        CGFloat(max(4, 18 - motion.shakeMagnitude * 8))
    }

    private var readoutColumn: some View {
        VStack(alignment: .leading, spacing: 6) {
            Text("Sensor")
                .font(.headline)
            labeledValue("Tilt", String(format: "%.0f°", motion.tiltDegrees))
            labeledValue("Shake", String(format: "%.2f m/s²", motion.shakeMagnitude))
            labeledValue("Gravity Y", String(format: "%.2f", motion.gravityDisplay.y))
            Text("Dot = tilt direction. Orange ring = shake.")
                .font(.caption2)
                .foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }

    private func labeledValue(_ title: String, _ value: String) -> some View {
        HStack {
            Text(title)
                .font(.caption)
                .foregroundStyle(.secondary)
            Spacer(minLength: 8)
            Text(value)
                .font(.caption.monospacedDigit())
        }
    }

    private var tuningSliders: some View {
        VStack(alignment: .leading, spacing: 12) {
            tuningSlider("Gravity smoothing", value: $motion.gravitySmoothing, range: 0.05 ... 0.5, format: "%.2f")
            tuningSlider("Shake smoothing", value: $motion.accelSmoothing, range: 0.05 ... 0.5, format: "%.2f")
            tuningSlider("Spring tilt blend", value: $motion.gravityBlend, range: 0 ... 1, format: "%.2f")
            tuningSlider("Max tilt (°)", value: $motion.maxTiltDegrees, range: 5 ... 85, format: "%.0f")
            tuningSlider("Shake power", value: $motion.shakePower, range: 0 ... 0.6, format: "%.2f")
            tuningSlider("Max shake ×", value: $motion.maxShakeMultiplier, range: 1 ... 6, format: "%.1f")
            tuningSlider("Shake deadzone", value: $motion.shakeDeadzone, range: 0 ... 0.5, format: "%.2f")

            Button("Reset defaults") {
                motion.resetTuningToDefaults()
            }
            .buttonStyle(.bordered)
        }
    }

    private func tuningSlider(
        _ title: String,
        value: Binding<Double>,
        range: ClosedRange<Double>,
        format: String
    ) -> some View {
        VStack(alignment: .leading, spacing: 2) {
            HStack {
                Text(title)
                    .font(.caption)
                Spacer()
                Text(String(format: format, value.wrappedValue))
                    .font(.caption2.monospacedDigit())
                    .foregroundStyle(.secondary)
            }
            Slider(value: value, in: range)
                .onChange(of: value.wrappedValue) { _, _ in
                    JarvisBevySession.pushDeviceMotionTuning()
                }
        }
    }
}
