// Monochrome theme — a port of crates/ui/src/theme.rs (dark) and of the
// desktop's built-in `luna-light` variant (crates/theme/src/builtins.rs).
//
// Dark colors are computed from the same oklch definitions the desktop app
// uses (Björn Ottosson's OKLab matrices, the ones CSS Color 4 specifies), so
// every surface and accent lands on identical sRGB values. Light colors are
// the `luna-light` seeds and the same derivation rules (black hairlines and
// washes where dark uses white). Every color resolves from the trait
// collection: a presentation that forces dark (the edge-backed screens)
// paints exactly the shipped dark palette, and the paired-Mac shell follows
// the user's System/Light/Dark preference (IOS-02). **Numbers drive layout,
// colors are paint**: layout constants are plain numbers and never depend on
// which color is painted.

import SwiftUI
import UIKit

enum Theme {
    // ---- paint: neutral surfaces (oklch chroma 0) ----
    /// Main panel background — sampled #060606 (light: #fbfcfe).
    static let bg = adaptive(dark: grey(6), light: hex(0xfbfcfe))
    /// Shell / sidebar surface — sampled #0d0d0d (light: #f0f3f7).
    static let surface = adaptive(dark: grey(13), light: hex(0xf0f3f7))
    /// Raised surface: popovers, dialogs, cards (light: #e7ecf2).
    static let surfaceRaised = adaptive(dark: neutral(0.235), light: hex(0xe7ecf2))
    /// Hover/pressed wash for interactive rows (low alpha).
    static let elementHover = whiteAlpha(0.06)
    /// Active/selected wash.
    static let elementActive = adaptive(dark: Color.white.opacity(0.10), light: Color.black.opacity(0.08))
    /// Hairline border — low alpha so it reads on any surface.
    static let border = adaptive(dark: Color.white.opacity(0.08), light: Color.black.opacity(0.12))
    /// Stronger border for focused/raised edges.
    static let borderStrong = adaptive(dark: Color.white.opacity(0.14), light: Color.black.opacity(0.22))

    // ---- paint: text ----
    static let text = adaptive(dark: neutral(0.922), light: hex(0x202631))       // ~neutral-200
    static let textMuted = adaptive(dark: neutral(0.708), light: hex(0x445064))  // ~neutral-400
    static let textFaint = adaptive(dark: neutral(0.556), light: hex(0x586579))  // ~neutral-500

    // ---- paint: accents ----
    static let accent = adaptive(dark: oklch(0.673, 0.182, 276.935), light: hex(0x5b43e8))        // indigo-400
    static let accentStrong = adaptive(dark: oklch(0.585, 0.233, 277.117), light: hex(0x5b43e8))  // indigo-500
    static let danger = adaptive(dark: oklch(0.704, 0.191, 22.216), light: hex(0xdc2626))         // red-400
    static let dangerSoft = adaptive(dark: oklch(0.808, 0.114, 19.571), light: hex(0xb91c1c))     // red-300
    static let warning = adaptive(dark: oklch(0.828, 0.189, 84.429), light: hex(0xa16207))        // amber-400

    // ---- paint: status dots (shell/spaces.rs status_dot_color) ----
    static let statusWorking = adaptive(dark: oklch(0.718, 0.202, 349.761), light: hex(0xbe185d))   // pink-400
    static let statusCompleted = adaptive(dark: oklch(0.765, 0.177, 163.223), light: hex(0x15803d)) // emerald-400
    /// Claude brand orange — kept even on the mono surface.
    static let claudeBrand = Color(red: 0xD9 / 255.0, green: 0x77 / 255.0, blue: 0x57 / 255.0)

    // ---- paint: markdown inline code (violet family) ----
    static let inlineCodeText = adaptive(dark: oklch(0.811, 0.111, 293.571), light: hex(0x7e22ce))  // violet-300
    static let inlineCodeWash = adaptive(dark: oklch(0.702, 0.183, 293.541).opacity(0.12),
                                         light: hex(0x7e22ce).opacity(0.10)) // violet-400 @ 0.12

    // ---- paint: syntax tokens (soft, paint-only) ----
    static let tokenKeyword = adaptive(dark: oklch(0.709, 0.129, 20.0), light: hex(0xbe185d))   // soft rose
    static let tokenString = adaptive(dark: oklch(0.770, 0.110, 168.0), light: hex(0x15803d))   // soft green
    static let tokenNumber = adaptive(dark: oklch(0.780, 0.120, 80.0), light: hex(0xa16207))    // soft amber

    // ---- numbers drive layout (pt) ----
    static let bubbleRadius: CGFloat = 22
    static let panelRadius: CGFloat = 10
    static let controlRadius: CGFloat = 6
    static let spaceXS: CGFloat = 4
    static let spaceSM: CGFloat = 8
    static let spaceMD: CGFloat = 12
    static let spaceLG: CGFloat = 16
}

// MARK: - Fonts

extension Theme {
    static let fontSansName = "Geist"
    static let fontMonoName = "GeistMono-Regular"

    static func sans(_ size: CGFloat, weight: Font.Weight = .regular) -> Font {
        // Static weight cuts register as separate families — select by
        // PostScript name so weights actually resolve.
        let name: String
        if weight == .medium {
            name = "Geist-Medium"
        } else if weight == .semibold {
            name = "Geist-SemiBold"
        } else if weight == .bold {
            name = "Geist-Bold"
        } else {
            name = "Geist-Regular"
        }
        return .custom(name, size: size)
    }

    static func mono(_ size: CGFloat, weight: Font.Weight = .regular) -> Font {
        .custom(fontMonoName, size: size).weight(weight)
    }

    static func sansUI(_ size: CGFloat, weight: UIFont.Weight = .regular) -> UIFont {
        let traits: [UIFontDescriptor.TraitKey: Any] = [.weight: weight]
        let descriptor = UIFontDescriptor(fontAttributes: [
            .family: "Geist",
            .traits: traits,
        ])
        return UIFont(descriptor: descriptor, size: size)
    }

    static func monoUI(_ size: CGFloat) -> UIFont {
        UIFont(name: fontMonoName, size: size)
            ?? .monospacedSystemFont(ofSize: size, weight: .regular)
    }
}

// MARK: - Color primitives (ported from theme.rs)

/// A neutral (chroma 0) oklch tone. Chroma 0 means r == g == b exactly.
func neutral(_ lightness: Double) -> Color {
    let v = Double(oklchToSrgb(l: lightness, c: 0, hDeg: 0)[0])
    return Color(red: v, green: v, blue: v)
}

/// The hairline/wash primitive: white at the given alpha on dark, black at
/// the same alpha on light (the desktop's `border_tone` rule).
func whiteAlpha(_ alpha: Double) -> Color {
    adaptive(dark: Color.white.opacity(alpha), light: Color.black.opacity(alpha))
}

/// One color per appearance, resolved from the trait collection at paint
/// time (SwiftUI environment or the UIKit view's traits). An unspecified
/// style paints dark, the app's historical look.
func adaptive(dark: Color, light: Color) -> Color {
    let darkColor = UIColor(dark)
    let lightColor = UIColor(light)
    return Color(uiColor: UIColor { traits in
        traits.userInterfaceStyle == .light ? lightColor : darkColor
    })
}

/// An opaque sRGB color from `0xRRGGBB` (the desktop theme's hex seeds).
func hex(_ value: UInt32) -> Color {
    Color(red: Double((value >> 16) & 0xff) / 255.0,
          green: Double((value >> 8) & 0xff) / 255.0,
          blue: Double(value & 0xff) / 255.0)
}

/// An exact achromatic tone from an 8-bit channel value (`grey(13)` ≡ #0d0d0d).
func grey(_ value: UInt8) -> Color {
    let v = Double(value) / 255.0
    return Color(red: v, green: v, blue: v)
}

/// oklch (CSS notation: L 0..1, C, H degrees) → sRGB Color.
func oklch(_ l: Double, _ c: Double, _ hDeg: Double) -> Color {
    let rgb = oklchToSrgb(l: l, c: c, hDeg: hDeg)
    return Color(red: Double(rgb[0]), green: Double(rgb[1]), blue: Double(rgb[2]))
}

/// oklch → sRGB (each 0..1, clamped/gamut-clipped per channel).
func oklchToSrgb(l: Double, c: Double, hDeg: Double) -> [Double] {
    let h = hDeg * .pi / 180
    let a = c * cos(h)
    let b = c * sin(h)

    // OKLab → LMS (cube roots undone)
    let l_ = l + 0.39633778 * a + 0.21580376 * b
    let m_ = l - 0.105561346 * a - 0.06385417 * b
    let s_ = l - 0.08948418 * a - 1.2914855 * b
    let (l3, m3, s3) = (l_ * l_ * l_, m_ * m_ * m_, s_ * s_ * s_)

    // LMS → linear sRGB
    let r = 4.0767417 * l3 - 3.3077116 * m3 + 0.23096993 * s3
    let g = -1.268438 * l3 + 2.6097574 * m3 - 0.3413194 * s3
    let bl = -0.0041960863 * l3 - 0.7034186 * m3 + 1.7076147 * s3

    return [gammaEncode(r), gammaEncode(g), gammaEncode(bl)]
}

private func gammaEncode(_ x: Double) -> Double {
    let x = min(max(x, 0), 1)
    return x <= 0.0031308 ? 12.92 * x : 1.055 * pow(x, 1.0 / 2.4) - 0.055
}
