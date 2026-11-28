// Shows a picture on one display of this Mac, one pixel of the picture to one pixel of the display, until
// the process ends. The window takes no focus and leaves the other displays alone. A test shows the
// sharpness pattern (pattern.swift) this way on a virtual display that only the headset sees.
//
// Usage: swift scripts/sharpness/show.swift PICTURE DISPLAY_ID [busy]
//   busy  a square moves in the top left corner 30 times a second, so the display changes as a busy
//         desktop does and the stream carries 30 pictures a second (for a measure of the costs)
import AppKit

let args = CommandLine.arguments
guard (3...4).contains(args.count), let id = UInt32(args[2]),
      let source = CGImageSourceCreateWithURL(URL(fileURLWithPath: args[1]) as CFURL, nil),
      let image = CGImageSourceCreateImageAtIndex(source, 0, nil) else {
    print("usage: swift scripts/sharpness/show.swift PICTURE DISPLAY_ID")
    exit(2)
}
let app = NSApplication.shared
// No Dock icon and no menu bar: the window must not take the user's focus
app.setActivationPolicy(.accessory)
guard let screen = NSScreen.screens.first(where: { ($0.deviceDescription[NSDeviceDescriptionKey("NSScreenNumber")] as? NSNumber)?.uint32Value == id }) else {
    print("no display \(id)")
    exit(1)
}
let scale = screen.backingScaleFactor
let pixels = (Int(screen.frame.width * scale), Int(screen.frame.height * scale))
guard pixels == (image.width, image.height) else {
    print("display \(id) has \(pixels.0) x \(pixels.1) pixels, the picture \(image.width) x \(image.height)")
    exit(1)
}
let window = NSWindow(contentRect: screen.frame, styleMask: .borderless, backing: .buffered, defer: false, screen: screen)
window.setFrame(screen.frame, display: false)
let view = NSView(frame: NSRect(origin: .zero, size: screen.frame.size))
view.wantsLayer = true
view.layer?.contents = image
view.layer?.contentsScale = scale
view.layer?.contentsGravity = .resize
view.layer?.magnificationFilter = .nearest
view.layer?.minificationFilter = .nearest
window.contentView = view
if args.count == 4 && args[3] == "busy" {
    let square = CALayer()
    square.backgroundColor = CGColor(gray: 1, alpha: 1)
    square.frame = CGRect(x: 0, y: screen.frame.height - 100, width: 40, height: 40)
    view.layer?.addSublayer(square)
    var step = 0
    let timer = Timer(timeInterval: 1.0 / 30.0, repeats: true) { _ in
        step = (step + 1) % 30
        CATransaction.begin()
        CATransaction.setDisableActions(true)
        square.frame.origin.x = CGFloat(step * 10)
        CATransaction.commit()
    }
    RunLoop.main.add(timer, forMode: .common)
}
window.orderFrontRegardless()
print("showing \(image.width) x \(image.height) on display \(id)")
fflush(stdout)
signal(SIGTERM) { _ in exit(0) }
app.run()
