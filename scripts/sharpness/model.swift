// A model of the headset's compositor, which samples a display's picture once more onto the headset's
// pixels (issue #222). It writes a picture of K times the input's size: each pixel takes TAPS x TAPS
// bilinear samples spread over its area of the input, one sample by default, as a compositor without
// super sampling takes. OFFSET moves the samples by a share of a pixel (0.31 by default), so the
// grid of the output does not meet the input's pixels by chance. measure.swift reads the output with
// --frame. The model is no capture of a headset: it compares the widths that the client may ask for.
//
// Usage: swift scripts/sharpness/model.swift IN.pgm OUT.pgm K [TAPS] [OFFSET]
//   IN.pgm  a picture of the probe (mac-check.sh); K the headset's pixels for each pixel of IN
import Foundation
let a = CommandLine.arguments
let data = FileManager.default.contents(atPath: a[1])!
let f = String(decoding: data.prefix(64), as: UTF8.self).split(whereSeparator: \.isWhitespace).prefix(4)
let (w, h) = (Int(f[1])!, Int(f[2])!)
let src = [UInt8](data.suffix(w * h)).map { Float($0) }
let k = Double(a[3])!, taps = a.count > 4 ? Int(a[4])! : 1, off = a.count > 5 ? Double(a[5])! : 0.31
let (ow, oh) = (Int((Double(w) * k).rounded()), Int((Double(h) * k).rounded()))
func at(_ x: Double, _ y: Double) -> Float {
  let fx = min(max(x - 0.5, 0), Double(w - 1)), fy = min(max(y - 0.5, 0), Double(h - 1))
  let x0 = Int(fx), y0 = Int(fy), x1 = min(x0 + 1, w - 1), y1 = min(y0 + 1, h - 1)
  let ax = Float(fx - Double(x0)), ay = Float(fy - Double(y0))
  return (src[y0*w+x0]*(1-ax) + src[y0*w+x1]*ax)*(1-ay) + (src[y1*w+x0]*(1-ax) + src[y1*w+x1]*ax)*ay
}
var out = [UInt8](repeating: 0, count: ow * oh)
let sx = Double(w) / Double(ow), sy = Double(h) / Double(oh)
for y in 0..<oh { for x in 0..<ow {
  var s: Float = 0
  for j in 0..<taps { for i in 0..<taps {
    let px = (Double(x) + off + (Double(i) + 0.5) / Double(taps)) * sx
    let py = (Double(y) + off + (Double(j) + 0.5) / Double(taps)) * sy
    s += at(px, py)
  }}
  out[y*ow+x] = UInt8(max(0, min(255, (s / Float(taps*taps)).rounded())))
}}
var file = Data("P5\n\(ow) \(oh)\n255\n".utf8); file.append(contentsOf: out)
try! file.write(to: URL(fileURLWithPath: a[2]))
