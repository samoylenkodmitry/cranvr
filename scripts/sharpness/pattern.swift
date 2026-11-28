// Makes the test pattern of a display panel's sharpness (issue #222) and writes it with its layout.
//
// The pattern is a desktop of 3840 x 2160 pixels at 1x, as a 4K display with its UI at 1x shows it. Two
// blocks stand at its middle: black on white on the left, white on black on the right. Each block holds
// gratings of lines one and two pixels wide and rows of text from 8 to 16 points, which macOS draws one
// pixel to a point at 1x. A colored row in each block shows what the chroma does to text. A colored
// square stands at each corner of the two blocks, so a script finds the pattern in any picture of it
// (scripts/sharpness/measure.swift). The rest is mid grey.
//
// Usage: swift scripts/sharpness/pattern.swift OUT_DIR
//   writes OUT_DIR/pattern.png and OUT_DIR/pattern.json (the place of each element in pixels)
import AppKit
import CoreGraphics
import CoreText
import Foundation
import ImageIO
import UniformTypeIdentifiers

let width = 3840, height = 2160
let args = CommandLine.arguments
guard args.count == 2 else {
    print("usage: swift scripts/sharpness/pattern.swift OUT_DIR")
    exit(2)
}
let out = URL(fileURLWithPath: args[1], isDirectory: true)
try FileManager.default.createDirectory(at: out, withIntermediateDirectories: true)

// The bitmap context draws with y up. The layout's rectangles use y down from the top, as a picture does.
let space = CGColorSpace(name: CGColorSpace.sRGB)!
let ctx = CGContext(data: nil, width: width, height: height, bitsPerComponent: 8, bytesPerRow: width * 4, space: space,
                    bitmapInfo: CGImageAlphaInfo.noneSkipLast.rawValue)!
ctx.setShouldAntialias(true)
ctx.setAllowsFontSmoothing(false)
ctx.interpolationQuality = .none

func fill(_ x: Int, _ y: Int, _ w: Int, _ h: Int, _ rgb: (CGFloat, CGFloat, CGFloat)) {
    ctx.setFillColor(red: rgb.0, green: rgb.1, blue: rgb.2, alpha: 1)
    ctx.fill(CGRect(x: x, y: height - y - h, width: w, height: h))
}

// Draws `text` with its baseline at pixel row `baseline` from the top and returns the width in pixels.
func draw(_ text: String, size: CGFloat, x: Int, baseline: Int, rgb: (CGFloat, CGFloat, CGFloat)) -> Double {
    let font = CTFontCreateUIFontForLanguage(.system, size, nil)!
    let color = CGColor(srgbRed: rgb.0, green: rgb.1, blue: rgb.2, alpha: 1)
    let attributes: [NSAttributedString.Key: Any] = [.font: font, .foregroundColor: color]
    let line = CTLineCreateWithAttributedString(NSAttributedString(string: text, attributes: attributes))
    ctx.textPosition = CGPoint(x: CGFloat(x), y: CGFloat(height - baseline))
    CTLineDraw(line, ctx)
    return CTLineGetTypographicBounds(line, nil, nil, nil)
}

// The row of "minimum" that fits in `room` pixels at `size`
func row(size: CGFloat, room: Int) -> String {
    let font = CTFontCreateUIFontForLanguage(.system, size, nil)!
    let word = CTLineCreateWithAttributedString(NSAttributedString(string: "minimum ", attributes: [.font: font]))
    return String(repeating: "minimum ", count: Int(Double(room) / CTLineGetTypographicBounds(word, nil, nil, nil))).trimmingCharacters(in: .whitespaces)
}

func xHeight(_ size: CGFloat) -> Double {
    Double(CTFontGetXHeight(CTFontCreateUIFontForLanguage(.system, size, nil)!))
}

var elements: [[String: Any]] = []
let grey: (CGFloat, CGFloat, CGFloat) = (0.5, 0.5, 0.5)
fill(0, 0, width, height, grey)

// The two blocks: left black on white, right white on black
let blockW = 800, blockH = 940, top = 610, gap = 20
let lefts = [width / 2 - gap / 2 - blockW, width / 2 + gap / 2]
let markerSize = 32
let markerColors: [(String, (CGFloat, CGFloat, CGFloat))] = [("red", (1, 0, 0)), ("green", (0, 1, 0)), ("blue", (0, 0, 1)), ("magenta", (1, 0, 1))]
let corners = [(lefts[0] - markerSize - 8, top - markerSize - 8), (lefts[1] + blockW + 8, top - markerSize - 8),
               (lefts[0] - markerSize - 8, top + blockH + 8), (lefts[1] + blockW + 8, top + blockH + 8)]
for ((name, rgb), (x, y)) in zip(markerColors, corners) {
    fill(x, y, markerSize, markerSize, rgb)
    elements.append(["kind": "marker", "color": name, "center": [Double(x) + Double(markerSize) / 2, Double(y) + Double(markerSize) / 2]])
}

let sizes: [CGFloat] = [8, 9, 10, 11, 12, 13, 14, 16]
for (index, left) in lefts.enumerated() {
    let dark = index == 1
    let paper: (CGFloat, CGFloat, CGFloat) = dark ? (0, 0, 0) : (1, 1, 1)
    let ink: (CGFloat, CGFloat, CGFloat) = dark ? (1, 1, 1) : (0, 0, 0)
    let polarity = dark ? "white_on_black" : "black_on_white"
    fill(left, top, blockW, blockH, paper)
    // The levels: the paper itself and a box of ink
    elements.append(["kind": "level", "polarity": polarity, "level": "paper", "rect": [left + 20, top + 20, 120, 60]])
    fill(left + 160, top + 20, 120, 60, ink)
    elements.append(["kind": "level", "polarity": polarity, "level": "ink", "rect": [left + 170, top + 30, 100, 40]])
    // Gratings: lines across (they vary along x) of 1 and 2 pixels, then lines along of 1 pixel
    let gy = top + 100
    for (n, (period, axis)) in [(2, "x"), (4, "x"), (2, "y")].enumerated() {
        let gx = left + 20 + n * 260
        let (gw, gh) = (240, 160)
        for i in 0..<(axis == "x" ? gw : gh) where i % period < period / 2 {
            if axis == "x" { fill(gx + i, gy, 1, gh, ink) } else { fill(gx, gy + i, gw, 1, ink) }
        }
        elements.append(["kind": "grating", "polarity": polarity, "period": period, "axis": axis, "rect": [gx, gy, gw, gh]])
    }
    // Rows of text, each a word whose letters are all stems and gaps
    var baseline = gy + 160 + 40
    for size in sizes {
        baseline += Int(size * 1.9)
        let w = draw(row(size: size, room: blockW - 40), size: size, x: left + 20, baseline: baseline, rgb: ink)
        let xh = xHeight(size)
        // The band through the middle of the x-height, where m, i, n and u are only stems
        let bandTop = Double(baseline) - xh * 0.7, bandH = max(1.0, xh * 0.4)
        elements.append(["kind": "text", "polarity": polarity, "size": Double(size), "color": dark ? "white" : "black",
                         "rect": [Double(left + 20), bandTop, w, bandH]])
    }
    // A colored row: blue on white, yellow on black, the colors that 4:2:0 blurs most
    baseline += 40
    let colored: (CGFloat, CGFloat, CGFloat) = dark ? (1, 1, 0) : (0, 0, 1)
    let w = draw(row(size: 12, room: blockW - 40), size: 12, x: left + 20, baseline: baseline, rgb: colored)
    elements.append(["kind": "text", "polarity": polarity, "size": 12.0, "color": dark ? "yellow" : "blue",
                     "rect": [Double(left + 20), Double(baseline) - xHeight(12) * 0.7, w, max(1.0, xHeight(12) * 0.4)]])
    // A line for a person's eye: the words of a call's toolbar at 12 points
    baseline += 40
    _ = draw("Zoom  Share  Chat  Participants  Record  Reactions", size: 12, x: left + 20, baseline: baseline, rgb: ink)
    baseline += 30
    _ = draw("Zoom  Share  Chat  Participants  Record  Reactions", size: 16, x: left + 20, baseline: baseline, rgb: ink)
}

let image = ctx.makeImage()!
let png = out.appendingPathComponent("pattern.png")
let dest = CGImageDestinationCreateWithURL(png as CFURL, UTType.png.identifier as CFString, 1, nil)!
CGImageDestinationAddImage(dest, image, nil)
guard CGImageDestinationFinalize(dest) else { print("pattern.png not written"); exit(1) }
let layout: [String: Any] = ["size": [width, height], "elements": elements]
let json = try JSONSerialization.data(withJSONObject: layout, options: [.prettyPrinted, .sortedKeys])
try json.write(to: out.appendingPathComponent("pattern.json"))
print("wrote \(png.path) and pattern.json: \(elements.count) elements")
