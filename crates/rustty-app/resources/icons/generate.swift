// Regenerate Rustty's checked-in app icons on macOS with Swift and system frameworks.
import CoreGraphics
import Foundation
import ImageIO
import UniformTypeIdentifiers

let output = URL(fileURLWithPath: #filePath).deletingLastPathComponent()
let root = output.appendingPathComponent("../../../..").standardizedFileURL
let assets = root.appendingPathComponent("macos/Assets.xcassets/Custom Icon")
let space = CGColorSpace(name: CGColorSpace.sRGB)!
let bounds = CGRect(x: 0, y: 0, width: 1024, height: 1024)

func color(_ hex: UInt32, alpha: CGFloat = 1) -> CGColor {
  CGColor(
    colorSpace: space,
    components: [
      CGFloat((hex >> 16) & 255) / 255,
      CGFloat((hex >> 8) & 255) / 255,
      CGFloat(hex & 255) / 255, alpha,
    ])!
}

func bitmap(_ size: Int = 1024) -> CGContext {
  CGContext(
    data: nil, width: size, height: size, bitsPerComponent: 8,
    bytesPerRow: size * 4, space: space,
    bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue)!
}

func layer(_ path: String) -> CGImage {
  let url = assets.appendingPathComponent(path)
  guard let source = CGImageSourceCreateWithURL(url as CFURL, nil),
    let image = CGImageSourceCreateImageAtIndex(source, 0, nil)
  else {
    fatalError("Cannot read icon layer: \(url.path)")
  }
  precondition(image.width == 1024 && image.height == 1024)
  return image
}

func gradient(_ context: CGContext, _ colors: [CGColor], from: CGPoint, to: CGPoint) {
  context.drawLinearGradient(
    CGGradient(colorsSpace: space, colors: colors as CFArray, locations: nil)!,
    start: from, end: to, options: [.drawsBeforeStartLocation, .drawsAfterEndLocation])
}

func png(_ image: CGImage) -> Data {
  let data = NSMutableData()
  let destination = CGImageDestinationCreateWithData(
    data, UTType.png.identifier as CFString, 1, nil)!
  CGImageDestinationAddImage(destination, image, nil)
  precondition(CGImageDestinationFinalize(destination), "Cannot encode PNG")
  return data as Data
}

let icon = bitmap()
icon.draw(layer("CustomIconBaseAluminum.imageset/base.png"), in: bounds)
icon.draw(layer("CustomIconScreen.imageset/screen-dark.png"), in: bounds)
icon.saveGState()
icon.clip(to: bounds, mask: layer("CustomIconScreenMask.imageset/screen-mask.png"))
icon.setBlendMode(.color)
gradient(
  icon, [color(0xA62C05), color(0x5B210E), color(0x25100B)],
  from: CGPoint(x: 512, y: 180), to: CGPoint(x: 512, y: 840))
icon.restoreGState()

// Original crab silhouette: a broad shell, raised pincers, and three legs per side.
let crab = bitmap()
let limbs = CGMutablePath()
for mirrored in [false, true] {
  var transform =
    mirrored
    ? CGAffineTransform(translationX: 1024, y: 0).scaledBy(x: -1, y: 1)
    : .identity
  let side = CGMutablePath()
  side.move(to: CGPoint(x: 392, y: 510))
  side.addCurve(
    to: CGPoint(x: 281, y: 505), control1: CGPoint(x: 346, y: 530),
    control2: CGPoint(x: 310, y: 526))
  side.move(to: CGPoint(x: 390, y: 488))
  side.addCurve(
    to: CGPoint(x: 302, y: 453), control1: CGPoint(x: 345, y: 493),
    control2: CGPoint(x: 318, y: 474))
  side.move(to: CGPoint(x: 406, y: 467))
  side.addCurve(
    to: CGPoint(x: 345, y: 420), control1: CGPoint(x: 368, y: 463),
    control2: CGPoint(x: 351, y: 444))
  side.move(to: CGPoint(x: 396, y: 545))
  side.addCurve(
    to: CGPoint(x: 330, y: 595), control1: CGPoint(x: 346, y: 544),
    control2: CGPoint(x: 330, y: 560))
  limbs.addPath(side, transform: transform)

  let claw = CGMutablePath()
  claw.move(to: CGPoint(x: 350, y: 582))
  claw.addCurve(
    to: CGPoint(x: 284, y: 655), control1: CGPoint(x: 298, y: 573),
    control2: CGPoint(x: 265, y: 615))
  claw.addCurve(
    to: CGPoint(x: 305, y: 677), control1: CGPoint(x: 289, y: 668),
    control2: CGPoint(x: 297, y: 675))
  claw.addLine(to: CGPoint(x: 325, y: 629))
  claw.addLine(to: CGPoint(x: 352, y: 676))
  claw.addCurve(
    to: CGPoint(x: 374, y: 648), control1: CGPoint(x: 365, y: 671),
    control2: CGPoint(x: 372, y: 660))
  claw.addCurve(
    to: CGPoint(x: 350, y: 582), control1: CGPoint(x: 388, y: 618),
    control2: CGPoint(x: 377, y: 593))
  claw.closeSubpath()
  crab.addPath(claw.copy(using: &transform)!)
  crab.setFillColor(color(0xFFFFFF))
  crab.fillPath()
}
crab.addPath(limbs)
crab.setStrokeColor(color(0xFFFFFF))
crab.setLineWidth(25)
crab.setLineCap(.round)
crab.strokePath()

let shell = CGMutablePath()
shell.move(to: CGPoint(x: 365, y: 503))
shell.addCurve(
  to: CGPoint(x: 512, y: 629), control1: CGPoint(x: 365, y: 580), control2: CGPoint(x: 428, y: 629))
shell.addCurve(
  to: CGPoint(x: 659, y: 503), control1: CGPoint(x: 596, y: 629), control2: CGPoint(x: 659, y: 580))
shell.addCurve(
  to: CGPoint(x: 512, y: 438), control1: CGPoint(x: 659, y: 453), control2: CGPoint(x: 585, y: 438))
shell.addCurve(
  to: CGPoint(x: 365, y: 503), control1: CGPoint(x: 439, y: 438), control2: CGPoint(x: 365, y: 453))
shell.closeSubpath()
crab.addPath(shell)
crab.fillPath()
let silhouette = crab.makeImage()!

icon.saveGState()
icon.setShadow(offset: .zero, blur: 28, color: color(0xFF7B28, alpha: 0.65))
icon.draw(silhouette, in: bounds)
icon.restoreGState()
icon.saveGState()
icon.clip(to: bounds, mask: silhouette)
gradient(
  icon, [color(0xF66324), color(0xFFB36A), color(0xFFE4B9)],
  from: CGPoint(x: 512, y: 420), to: CGPoint(x: 512, y: 680))
icon.restoreGState()

icon.setFillColor(color(0x37170E))
for x in [458, 541] {
  icon.fillEllipse(in: CGRect(x: x, y: 519, width: 25, height: 43))
}

icon.setBlendMode(.overlay)
icon.draw(layer("CustomIconCRT.imageset/crt-effect.png"), in: bounds)
icon.setBlendMode(.normal)
icon.draw(layer("CustomIconGloss.imageset/gloss.png"), in: bounds)
let image = icon.makeImage()!
try png(image).write(to: output.appendingPathComponent("rustty.png"))

// PNG-backed ICO frames work on all supported Windows versions.
let sizes = [16, 20, 24, 32, 40, 48, 64, 128, 256]
let frames = sizes.map { size -> Data in
  let context = bitmap(size)
  context.interpolationQuality = .high
  context.draw(image, in: CGRect(x: 0, y: 0, width: size, height: size))
  return png(context.makeImage()!)
}
var ico = Data()
func word<T: FixedWidthInteger>(_ value: T) {
  withUnsafeBytes(of: value.littleEndian) { ico.append(contentsOf: $0) }
}
word(UInt16(0))
word(UInt16(1))
word(UInt16(sizes.count))
var offset = 6 + 16 * sizes.count
for (size, frame) in zip(sizes, frames) {
  ico.append(contentsOf: [UInt8(size % 256), UInt8(size % 256), 0, 0])
  word(UInt16(1))
  word(UInt16(32))
  word(UInt32(frame.count))
  word(UInt32(offset))
  offset += frame.count
}
for frame in frames { ico.append(frame) }
try ico.write(to: output.appendingPathComponent("rustty.ico"))
print("Generated rustty.png (1024×1024) and rustty.ico (\(sizes.count) sizes)")
