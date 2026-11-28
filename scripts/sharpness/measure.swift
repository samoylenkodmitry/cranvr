// Measures how sharp the test pattern of scripts/sharpness/pattern.swift is in a picture of it: a headset
// capture, a decoded picture of the display's stream or the pattern itself (issue #222).
//
// The script finds the four colored squares of the pattern and maps the pattern onto the picture with the
// projection that they give. Of the patches of each square's color, it takes the four whose places keep
// the pattern's shape. It reads the picture's luma along each element of the pattern and prints:
//   - scale: the picture's pixels to one pixel of the pattern, across;
//   - the contrast of each grating (lines of 1 and 2 pixels) as a share of the pattern's own contrast:
//     the amplitude of the grating's frequency over the picture's range from ink to paper;
//   - for each row of text, the share of the gaps between stems that stay open (the gap is at least 25 %
//     of the range from ink to paper lighter than the stems beside it, for dark ink) and the median gap;
//   - the smallest text size whose letters stay apart: the smallest size from which every larger black
//     or white row keeps 90 % of its gaps open.
// The pattern itself gives 100 % for every grating. A gap that is closed in the pattern counts as closed.
//
// Usage: swift scripts/sharpness/measure.swift PICTURE PATTERN_DIR [--eye left | --frame] [--json OUT]
//   PICTURE   a PNG, a raw screencap of `adb exec-out screencap` (no -p) or a PGM of luma
//   --eye     looks only at the left half of a headset capture, the left eye
//   --frame   the picture is the whole display at any size, such as a decoded picture of its stream:
//             the script maps the pattern by the sizes and looks for no squares
//   --json    writes the numbers to OUT as JSON as well
// It exits 3 when it does not find the pattern: no four patches keep its shape within 5 %, or a block's
// ink and paper differ by less than 40 levels of 255.
import CoreGraphics
import Foundation
import ImageIO

struct Picture {
    let width: Int, height: Int
    var luma: [Float]
    var rgb: [UInt8]

    init?(path: String) {
        guard let data = FileManager.default.contents(atPath: path), data.count > 16 else { return nil }
        var w = 0, h = 0
        var bytes: [UInt8]
        if data.starts(with: [0x50, 0x35]) {
            // A binary PGM of luma, as the probe writes it (`--save-display`)
            let fields = String(decoding: data.prefix(64), as: UTF8.self).split(whereSeparator: \.isWhitespace).prefix(4)
            guard fields.count == 4, let fw = Int(fields[1]), let fh = Int(fields[2]) else { return nil }
            (w, h) = (fw, fh)
            let luma = [UInt8](data.suffix(w * h))
            bytes = [UInt8](repeating: 255, count: w * h * 4)
            for i in 0..<(w * h) { (bytes[4 * i], bytes[4 * i + 1], bytes[4 * i + 2]) = (luma[i], luma[i], luma[i]) }
        } else if data.starts(with: [0x89, 0x50, 0x4e, 0x47]) {
            guard let src = CGImageSourceCreateWithData(data as CFData, nil), let img = CGImageSourceCreateImageAtIndex(src, 0, nil) else { return nil }
            (w, h) = (img.width, img.height)
            bytes = [UInt8](repeating: 0, count: w * h * 4)
            let ctx = CGContext(data: &bytes, width: w, height: h, bitsPerComponent: 8, bytesPerRow: w * 4,
                                space: CGColorSpace(name: CGColorSpace.sRGB)!, bitmapInfo: CGImageAlphaInfo.noneSkipLast.rawValue)!
            ctx.draw(img, in: CGRect(x: 0, y: 0, width: w, height: h))
        } else {
            // A raw screencap: width, height and format as 32-bit words, sometimes a color space, then RGBA
            let words = data.withUnsafeBytes { Array($0.bindMemory(to: UInt32.self).prefix(4)) }
            (w, h) = (Int(words[0]), Int(words[1]))
            let header = data.count - w * h * 4
            guard header == 12 || header == 16 else { return nil }
            bytes = [UInt8](data.subdata(in: header..<data.count))
        }
        (width, height, rgb) = (w, h, bytes)
        luma = [Float](repeating: 0, count: w * h)
        for i in 0..<(w * h) {
            luma[i] = 0.2126 * Float(bytes[4 * i]) + 0.7152 * Float(bytes[4 * i + 1]) + 0.0722 * Float(bytes[4 * i + 2])
        }
    }

    // The luma at a point between pixel centers, by bilinear interpolation
    func sample(_ x: Double, _ y: Double) -> Float {
        let fx = min(max(x - 0.5, 0), Double(width - 1)), fy = min(max(y - 0.5, 0), Double(height - 1))
        let x0 = Int(fx), y0 = Int(fy)
        let x1 = min(x0 + 1, width - 1), y1 = min(y0 + 1, height - 1)
        let ax = Float(fx - Double(x0)), ay = Float(fy - Double(y0))
        let top = luma[y0 * width + x0] * (1 - ax) + luma[y0 * width + x1] * ax
        let bottom = luma[y1 * width + x0] * (1 - ax) + luma[y1 * width + x1] * ax
        return top * (1 - ay) + bottom * ay
    }
}

// A projection of the plane: the pattern's pixels to the picture's pixels
struct Homography {
    var h: [Double]

    init?(from: [(Double, Double)], to: [(Double, Double)]) {
        var a = [[Double]](repeating: [Double](repeating: 0, count: 9), count: 8)
        for i in 0..<4 {
            let (x, y) = from[i], (u, v) = to[i]
            a[2 * i] = [x, y, 1, 0, 0, 0, -u * x, -u * y, u]
            a[2 * i + 1] = [0, 0, 0, x, y, 1, -v * x, -v * y, v]
        }
        for col in 0..<8 {
            guard let pivot = (col..<8).max(by: { abs(a[$0][col]) < abs(a[$1][col]) }), abs(a[pivot][col]) > 1e-12 else { return nil }
            a.swapAt(col, pivot)
            for row in 0..<8 where row != col {
                let f = a[row][col] / a[col][col]
                for k in col..<9 { a[row][k] -= f * a[col][k] }
            }
        }
        h = (0..<8).map { a[$0][8] / a[$0][$0] } + [1]
    }

    func map(_ x: Double, _ y: Double) -> (Double, Double) {
        let w = h[6] * x + h[7] * y + h[8]
        return ((h[0] * x + h[1] * y + h[2]) / w, (h[3] * x + h[4] * y + h[5]) / w)
    }
}

// Finds the patches of a marker's color, the largest first: their centers and sizes in pixels
func findMarkers(_ picture: Picture, color: String, columns: Range<Int>) -> [(Double, Double, Int)] {
    let (w, h) = (picture.width, picture.height)
    func matches(_ i: Int) -> Bool {
        let r = Int(picture.rgb[4 * i]), g = Int(picture.rgb[4 * i + 1]), b = Int(picture.rgb[4 * i + 2])
        switch color {
        case "red": return r > 120 && g < 90 && b < 90 && r - max(g, b) > 70
        case "green": return g > 120 && r < 90 && b < 90 && g - max(r, b) > 70
        case "blue": return b > 120 && r < 90 && g < 90 && b - max(r, g) > 70
        default: return r > 120 && b > 120 && g < 90 && min(r, b) - g > 70
        }
    }
    var seen = [Bool](repeating: false, count: w * h)
    var found: [(Double, Double, Int)] = []
    var stack: [Int] = []
    for y in 0..<h {
        for x in columns where !seen[y * w + x] {
            let start = y * w + x
            seen[start] = true
            guard matches(start) else { continue }
            stack = [start]
            var (sx, sy, n) = (0.0, 0.0, 0)
            while let i = stack.popLast() {
                let (px, py) = (i % w, i / w)
                sx += Double(px) + 0.5
                sy += Double(py) + 0.5
                n += 1
                for (nx, ny) in [(px - 1, py), (px + 1, py), (px, py - 1), (px, py + 1)] where columns.contains(nx) && ny >= 0 && ny < h {
                    let j = ny * w + nx
                    if !seen[j] {
                        seen[j] = true
                        if matches(j) { stack.append(j) }
                    }
                }
            }
            if n >= 4 { found.append((sx / Double(n), sy / Double(n), n)) }
        }
    }
    return Array(found.sorted { $0.2 > $1.2 }.prefix(3))
}

// How far four points stand from the best affine map of the layout's four points, as a share of the
// distance between the first two: a few hundredths for the pattern's squares seen in perspective
func shapeError(_ from: [(Double, Double)], _ to: [(Double, Double)]) -> Double {
    // Least squares of u = a x + b y + c and of v alike, by the normal equations
    func fit(_ target: [Double]) -> [Double] {
        var m = [[Double]](repeating: [0, 0, 0, 0], count: 3)
        for (i, (x, y)) in from.enumerated() {
            let row = [x, y, 1.0]
            for r in 0..<3 {
                for c in 0..<3 { m[r][c] += row[r] * row[c] }
                m[r][3] += row[r] * target[i]
            }
        }
        for col in 0..<3 {
            let pivot = (col..<3).max { abs(m[$0][col]) < abs(m[$1][col]) }!
            m.swapAt(col, pivot)
            for row in 0..<3 where row != col {
                let f = m[row][col] / m[col][col]
                for k in col..<4 { m[row][k] -= f * m[col][k] }
            }
        }
        return (0..<3).map { m[$0][3] / m[$0][$0] }
    }
    let (a, b) = (fit(to.map(\.0)), fit(to.map(\.1)))
    let error = zip(from, to).map { p, q in hypot(a[0] * p.0 + a[1] * p.1 + a[2] - q.0, b[0] * p.0 + b[1] * p.1 + b[2] - q.1) }.max()!
    return error / max(hypot(to[1].0 - to[0].0, to[1].1 - to[0].1), 1)
}

// The pattern in a picture: where it lies and how its elements read
struct View {
    let picture: Picture
    let map: Homography

    func at(_ x: Double, _ y: Double) -> Float {
        let (u, v) = map.map(x, y)
        return picture.sample(u, v)
    }

    // The mean luma over a rectangle of the pattern
    func mean(_ r: [Double]) -> Float {
        var (sum, n) = (Float(0), 0)
        for y in stride(from: r[1] + 0.5, to: r[1] + r[3], by: 1.0) {
            for x in stride(from: r[0] + 0.5, to: r[0] + r[2], by: 1.0) {
                sum += at(x, y)
                n += 1
            }
        }
        return sum / Float(max(n, 1))
    }

    // The luma along x through a band of the pattern, at `step` pixels, averaged over the band's height,
    // moved by `dx` pixels
    func rowProfile(_ r: [Double], step: Double, dx: Double = 0) -> [Float] {
        let rows = stride(from: r[1] + 0.25, to: r[1] + max(r[3], 0.5), by: 0.5).map { $0 }
        return stride(from: r[0], to: r[0] + r[2], by: step).map { x in
            rows.reduce(Float(0)) { $0 + at(x + dx, $1) } / Float(rows.count)
        }
    }

    // The luma across a grating, averaged along its lines
    func gratingProfile(_ r: [Double], axis: String, step: Double) -> [Float] {
        let inset = 8.0
        if axis == "x" {
            let ys = stride(from: r[1] + inset, to: r[1] + r[3] - inset, by: 1.0).map { $0 }
            return stride(from: r[0] + inset, to: r[0] + r[2] - inset, by: step).map { x in ys.reduce(Float(0)) { $0 + at(x, $1) } / Float(ys.count) }
        }
        let xs = stride(from: r[0] + inset, to: r[0] + r[2] - inset, by: 1.0).map { $0 }
        return stride(from: r[1] + inset, to: r[1] + r[3] - inset, by: step).map { y in xs.reduce(Float(0)) { $0 + at($1, y) } / Float(xs.count) }
    }
}

// The amplitude of one frequency (cycles a pattern pixel) in a profile sampled every `step` pixels
func amplitude(_ p: [Float], frequency: Double, step: Double) -> Double {
    var (re, im) = (0.0, 0.0)
    let mean = Double(p.reduce(0, +)) / Double(p.count)
    for (k, v) in p.enumerated() {
        let phase = 2 * Double.pi * frequency * Double(k) * step
        re += (Double(v) - mean) * cos(phase)
        im += (Double(v) - mean) * sin(phase)
    }
    return 2 * (re * re + im * im).squareRoot() / Double(p.count)
}

func correlation(_ a: [Float], _ b: [Float]) -> Double {
    let n = min(a.count, b.count)
    guard n > 2 else { return 0 }
    let ma = Double(a.prefix(n).reduce(0, +)) / Double(n), mb = Double(b.prefix(n).reduce(0, +)) / Double(n)
    var (ab, aa, bb) = (0.0, 0.0, 0.0)
    for i in 0..<n {
        let (x, y) = (Double(a[i]) - ma, Double(b[i]) - mb)
        ab += x * y
        aa += x * x
        bb += y * y
    }
    return ab / max((aa * bb).squareRoot(), 1e-9)
}

// The gaps between the stems of a row: (gap center, left stem center, right stem center) in samples.
// Runs of light longer than `space` samples are spaces between words and do not count.
func gaps(_ p: [Float], darkInk: Bool, space: Int) -> [(Int, Int, Int)] {
    guard let lo = p.min(), let hi = p.max(), hi - lo > 1 else { return [] }
    let threshold = (lo + hi) / 2
    let ink = p.map { darkInk ? $0 < threshold : $0 > threshold }
    var runs: [(Bool, Int, Int)] = []
    var start = 0
    for i in 1...ink.count where i == ink.count || ink[i] != ink[start] {
        runs.append((ink[start], start, i))
        start = i
    }
    var out: [(Int, Int, Int)] = []
    for i in 1..<max(runs.count - 1, 1) where !runs[i].0 && runs[i - 1].0 && runs[i + 1].0 && runs[i].2 - runs[i].1 <= space {
        let center = { (r: (Bool, Int, Int)) in (r.1 + r.2 - 1) / 2 }
        out.append((center(runs[i]), center(runs[i - 1]), center(runs[i + 1])))
    }
    return out
}

// How far a gap stands out from its stems, as a share of the range from ink to paper
func dips(_ p: [Float], at found: [(Int, Int, Int)], darkInk: Bool, range: Float, window: Int) -> [Double] {
    func pick(_ c: Int, light: Bool) -> Float {
        let part = p[max(c - window, 0)...min(c + window, p.count - 1)]
        return (light == darkInk ? part.max() : part.min()) ?? 0
    }
    return found.map { g, l, r in
        let gap = pick(g, light: true), stems = (pick(l, light: false) + pick(r, light: false)) / 2
        return Double(darkInk ? gap - stems : stems - gap) / Double(max(range, 1))
    }
}

let args = CommandLine.arguments
guard args.count >= 3 else {
    print("usage: swift scripts/sharpness/measure.swift PICTURE PATTERN_DIR [--eye left] [--json OUT]")
    exit(2)
}
let patternDir = URL(fileURLWithPath: args[2], isDirectory: true)
let leftEye = args.contains("--eye")
let wholeFrame = args.contains("--frame")
let jsonOut = args.firstIndex(of: "--json").map { args[$0 + 1] }
guard let picture = Picture(path: args[1]), let reference = Picture(path: patternDir.appendingPathComponent("pattern.png").path),
      let layoutData = FileManager.default.contents(atPath: patternDir.appendingPathComponent("pattern.json").path),
      let layout = try? JSONSerialization.jsonObject(with: layoutData) as? [String: Any],
      let elements = layout["elements"] as? [[String: Any]] else {
    print("unreadable picture or pattern")
    exit(2)
}
let columns = 0..<(leftEye ? picture.width / 2 : picture.width)
let markers = elements.filter { $0["kind"] as? String == "marker" }
let from: [(Double, Double)] = markers.map { let c = $0["center"] as! [Double]; return (c[0], c[1]) }
var to: [(Double, Double)] = []
if wholeFrame, let size = layout["size"] as? [Double] {
    to = from.map { ($0.0 * Double(picture.width) / size[0], $0.1 * Double(picture.height) / size[1]) }
} else {
    // A picture may hold other patches of the squares' colors, such as the host's scene in the room
    // around the panel: of the largest patches of each color, the four that stand where the
    // pattern's squares stand count
    var candidates: [[(Double, Double, Int)]] = []
    for marker in markers {
        let color = marker["color"] as! String
        let found = findMarkers(picture, color: color, columns: columns)
        guard !found.isEmpty else {
            print("no pattern: the \(color) square is missing")
            exit(3)
        }
        candidates.append(found)
    }
    var best: (error: Double, points: [(Double, Double)])?
    for a in candidates[0] { for b in candidates[1] { for c in candidates[2] { for d in candidates[3] {
        let points = [a, b, c, d].map { ($0.0, $0.1) }
        let error = shapeError(from, points)
        if error < (best?.error ?? .infinity) { best = (error, points) }
    }}}}
    guard let best, best.error < 0.05 else {
        print(String(format: "no pattern: the colored squares stand %.0f %% off the pattern's shape", 100 * (best?.error ?? 1)))
        exit(3)
    }
    to = best.points
}
guard let map = Homography(from: from, to: to) else {
    print("no pattern: the squares do not span a plane")
    exit(3)
}
let view = View(picture: picture, map: map)
let identity = View(picture: reference, map: Homography(from: from, to: from)!)
let scale = hypot(to[1].0 - to[0].0, to[1].1 - to[0].1) / hypot(from[1].0 - from[0].0, from[1].1 - from[0].1)

var results: [String: Any] = ["scale": scale]
var lines: [String] = [String(format: "scale %.3f picture pixels to a pattern pixel; %d x %d picture", scale, picture.width, picture.height)]
for polarity in ["black_on_white", "white_on_black"] {
    let darkInk = polarity == "black_on_white"
    let mine = elements.filter { $0["polarity"] as? String == polarity }
    let level = { (v: View, which: String) -> Float in
        v.mean(mine.first { $0["kind"] as? String == "level" && $0["level"] as? String == which }!["rect"] as! [Double])
    }
    let (paper, ink) = (level(view, "paper"), level(view, "ink"))
    let (refPaper, refInk) = (level(identity, "paper"), level(identity, "ink"))
    let range = abs(paper - ink), refRange = abs(refPaper - refInk)
    // The paper and the ink of the pattern differ by far more: a smaller range is another picture
    guard range >= 40 else {
        print("no pattern: the \(polarity) block's ink and paper differ by \(Int(range)) levels")
        exit(3)
    }
    var out: [String: Any] = ["paper": paper, "ink": ink]
    var gratingText: [String] = []
    for g in mine where g["kind"] as? String == "grating" {
        let rect = g["rect"] as! [Double], period = Double(g["period"] as! Int), axis = g["axis"] as! String
        let step = 0.25
        let a = amplitude(view.gratingProfile(rect, axis: axis, step: step), frequency: 1 / period, step: step)
        let ar = amplitude(identity.gratingProfile(rect, axis: axis, step: step), frequency: 1 / period, step: step)
        let contrast = (a / Double(max(range, 1))) / (ar / Double(refRange))
        let name = "lines \(Int(period / 2)) px \(axis == "x" ? "across" : "along")"
        out[name] = contrast
        gratingText.append(String(format: "%@ %.0f%%", name, 100 * contrast))
    }
    var rows: [[String: Any]] = []
    var rowText: [String] = []
    for t in mine where t["kind"] as? String == "text" {
        let rect = t["rect"] as! [Double], size = t["size"] as! Double, color = t["color"] as! String
        let step = 0.25
        let ref = identity.rowProfile(rect, step: step)
        // The projection of four squares may stand a pixel off here: the best match within 2 pixels counts
        let shifts = stride(from: -2.0, through: 2.0, by: 0.125).map { $0 }
        let best = shifts.max { correlation(view.rowProfile(rect, step: step, dx: $0), ref) < correlation(view.rowProfile(rect, step: step, dx: $1), ref) }!
        let profile = view.rowProfile(rect, step: step, dx: best)
        let ink = color == "black" || color == "blue" ? true : color == "white" || color == "yellow" ? false : darkInk
        let found = gaps(ref, darkInk: ink, space: Int(0.3 * size / step))
        let window = Int(0.5 / step)
        let d = dips(profile, at: found, darkInk: ink, range: range, window: window)
        let refD = dips(ref, at: found, darkInk: ink, range: refRange, window: window)
        let open = d.isEmpty ? 0 : Double(d.filter { $0 >= 0.25 }.count) / Double(d.count)
        let median = d.sorted()[d.count / 2]
        rows.append(["size": size, "color": color, "gaps": d.count, "open": open, "median_gap": median, "pattern_open": Double(refD.filter { $0 >= 0.25 }.count) / Double(max(refD.count, 1))])
        rowText.append(String(format: "%@ %.0f: %.0f%% open (gap %.0f%%)", color, size, 100 * open, 100 * median))
    }
    let plain = rows.filter { ["black", "white"].contains($0["color"] as! String) }.sorted { ($0["size"] as! Double) < ($1["size"] as! Double) }
    let smallest = plain.indices.first { i in plain[i...].allSatisfy { ($0["open"] as! Double) >= 0.9 } }.map { plain[$0]["size"] as! Double }
    out["rows"] = rows
    out["smallest_apart"] = smallest ?? NSNull()
    results[polarity] = out
    lines.append("\(polarity): ink \(Int(ink)), paper \(Int(paper)); " + gratingText.joined(separator: ", "))
    lines.append("  letters apart from " + (smallest.map { String(format: "%.0f pt", $0) } ?? "no size") + "; " + rowText.joined(separator: "; "))
}
print(lines.joined(separator: "\n"))
if let jsonOut {
    let data = try JSONSerialization.data(withJSONObject: results, options: [.prettyPrinted, .sortedKeys])
    try data.write(to: URL(fileURLWithPath: jsonOut))
}
