// Lists the modes of the first external display, or sets one for this process only.
// Usage: swift scripts/perf/display-modes.swift list
//        swift scripts/perf/display-modes.swift set WIDTH PIXELWIDTH   (holds the mode until stdin ends, then macOS
//                                                  restores the display: the change is for this
//                                                  process only)
import CoreGraphics
import Foundation

func mainExternal() -> CGDirectDisplayID {
    var ids = [CGDirectDisplayID](repeating: 0, count: 8)
    var n: UInt32 = 0
    CGGetOnlineDisplayList(8, &ids, &n)
    return ids.prefix(Int(n)).first { CGDisplayIsBuiltin($0) == 0 && CGDisplayIsActive($0) != 0 } ?? CGMainDisplayID()
}

let display = mainExternal()
let options = [kCGDisplayShowDuplicateLowResolutionModes: kCFBooleanTrue] as CFDictionary
let modes = (CGDisplayCopyAllDisplayModes(display, options) as? [CGDisplayMode]) ?? []
let current = CGDisplayCopyDisplayMode(display)!
func describe(_ m: CGDisplayMode) -> String {
    "\(m.width)x\(m.height) points, \(m.pixelWidth)x\(m.pixelHeight) pixels, \(m.refreshRate) Hz, gui \(m.isUsableForDesktopGUI())"
}
let args = CommandLine.arguments
if args.count > 1 && args[1] == "list" {
    print("display \(display) now: \(describe(current))")
    for m in modes where m.isUsableForDesktopGUI() { print("  " + describe(m)) }
    exit(0)
}
if args.count > 3 && args[1] == "set", let w = Int(args[2]), let pw = Int(args[3]) {
    guard let mode = modes.filter({ $0.width == w && $0.pixelWidth == pw && $0.isUsableForDesktopGUI() })
        .max(by: { abs($0.refreshRate - 60) > abs($1.refreshRate - 60) }) else {
        print("no such mode"); exit(1)
    }
    var config: CGDisplayConfigRef?
    CGBeginDisplayConfiguration(&config)
    CGConfigureDisplayWithDisplayMode(config, display, mode, nil)
    let err = CGCompleteDisplayConfiguration(config, .forAppOnly)
    print("set \(describe(mode)): \(err == .success ? "ok" : "error \(err.rawValue)")")
    fflush(stdout)
    while readLine() != nil {}
    print("restoring")
    exit(0)
}
print("usage: list | set WIDTH PIXELWIDTH")
