import SwiftUI

/// Obsidian Desk Dark / Swiss Ivory Light palette matching the PolyDirect desktop tokens.
enum PolyTheme {
    static func canvasBg(_ s: ColorScheme) -> Color {
        s == .light
            ? Color(red: 243 / 255, green: 245 / 255, blue: 248 / 255)
            : Color(red: 8 / 255, green: 11 / 255, blue: 16 / 255)
    }

    static func panelBg(_ s: ColorScheme) -> Color {
        s == .light
            ? Color.white
            : Color(red: 14 / 255, green: 18 / 255, blue: 25 / 255)
    }

    static func cardBg(_ s: ColorScheme) -> Color {
        s == .light
            ? Color.white
            : Color(red: 19 / 255, green: 24 / 255, blue: 34 / 255)
    }

    static func subBg(_ s: ColorScheme) -> Color {
        s == .light
            ? Color(red: 247 / 255, green: 249 / 255, blue: 252 / 255)
            : Color(red: 11 / 255, green: 15 / 255, blue: 22 / 255)
    }

    static func chipBg(_ s: ColorScheme) -> Color {
        s == .light
            ? Color(red: 237 / 255, green: 240 / 255, blue: 245 / 255)
            : Color(red: 24 / 255, green: 31 / 255, blue: 43 / 255)
    }

    static func border(_ s: ColorScheme) -> Color {
        s == .light
            ? Color(red: 226 / 255, green: 231 / 255, blue: 239 / 255)
            : Color(red: 30 / 255, green: 38 / 255, blue: 52 / 255)
    }

    static func borderStrong(_ s: ColorScheme) -> Color {
        s == .light
            ? Color(red: 201 / 255, green: 210 / 255, blue: 223 / 255)
            : Color(red: 46 / 255, green: 58 / 255, blue: 77 / 255)
    }

    static func textMuted(_ s: ColorScheme) -> Color {
        s == .light
            ? Color(red: 90 / 255, green: 101 / 255, blue: 119 / 255)
            : Color(red: 139 / 255, green: 152 / 255, blue: 171 / 255)
    }

    static func emerald(_ s: ColorScheme) -> Color {
        s == .light
            ? Color(red: 5 / 255, green: 150 / 255, blue: 105 / 255)
            : Color(red: 14 / 255, green: 203 / 255, blue: 129 / 255)
    }

    static func emeraldText(_ s: ColorScheme) -> Color {
        s == .light
            ? Color(red: 4 / 255, green: 120 / 255, blue: 87 / 255)
            : Color(red: 74 / 255, green: 222 / 255, blue: 162 / 255)
    }

    static func crimson(_ s: ColorScheme) -> Color {
        s == .light
            ? Color(red: 220 / 255, green: 38 / 255, blue: 38 / 255)
            : Color(red: 246 / 255, green: 70 / 255, blue: 93 / 255)
    }

    static func crimsonText(_ s: ColorScheme) -> Color {
        s == .light
            ? Color(red: 185 / 255, green: 28 / 255, blue: 28 / 255)
            : Color(red: 252 / 255, green: 134 / 255, blue: 150 / 255)
    }

    static func amber(_ s: ColorScheme) -> Color {
        s == .light
            ? Color(red: 180 / 255, green: 83 / 255, blue: 9 / 255)
            : Color(red: 245 / 255, green: 176 / 255, blue: 65 / 255)
    }

    static func azure(_ s: ColorScheme) -> Color {
        s == .light
            ? Color(red: 37 / 255, green: 99 / 255, blue: 235 / 255)
            : Color(red: 59 / 255, green: 130 / 255, blue: 246 / 255)
    }
}
