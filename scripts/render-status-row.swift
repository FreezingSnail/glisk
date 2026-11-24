// Render one terminal status row for the README captures.
//
// The pager writes its status row as plain text, and a compositor screenshot
// is unavailable on the capture host. This tool draws that exact text with the
// recorded terminal font and cell geometry, so a composed capture still shows
// the characters the running pager produced.
//
//   swiftc -O scripts/render-status-row.swift -o /tmp/render-status-row
//   /tmp/render-status-row <status.txt> <font.otf> <width-px> <cell-w> <cell-h> \
//       <bg-hex> <fg-hex> <out.png>
//
// The font size is solved so one advance equals the recorded cell width; the
// baseline centers the font's ascent and descent in the cell.

import CoreGraphics
import CoreText
import Foundation
import ImageIO
import UniformTypeIdentifiers

func fail(_ message: String) -> Never {
    FileHandle.standardError.write(Data((message + "\n").utf8))
    exit(1)
}

let args = CommandLine.arguments
guard args.count == 9 else {
    fail("usage: render-status-row <status.txt> <font.otf> <width-px> <cell-w> <cell-h> <bg> <fg> <out.png>")
}
let statusPath = args[1]
let fontPath = args[2]
let imageWidth = Int(args[3])!
let cellWidth = Double(args[4])!
let cellHeight = Double(args[5])!

func color(_ hex: String) -> CGColor {
    var value: UInt64 = 0
    let digits = hex.hasPrefix("#") ? String(hex.dropFirst()) : hex
    Scanner(string: digits).scanHexInt64(&value)
    let red = Double((value >> 16) & 0xff) / 255
    let green = Double((value >> 8) & 0xff) / 255
    let blue = Double(value & 0xff) / 255
    return CGColor(
        colorSpace: CGColorSpaceCreateDeviceRGB(),
        components: [red, green, blue, 1]
    )!
}

let background = color(args[6])
let foreground = color(args[7])
let outputPath = args[8]

guard let status = try? String(contentsOfFile: statusPath, encoding: .utf8) else {
    fail("cannot read \(statusPath)")
}
let text = status.trimmingCharacters(in: .newlines)

let fontURL = URL(fileURLWithPath: fontPath)
CTFontManagerRegisterFontsForURL(fontURL as CFURL, .process, nil)
let descriptor = CTFontDescriptorCreateWithAttributes(
    [kCTFontURLAttribute: fontURL] as CFDictionary
)
let probe = CTFontCreateWithFontDescriptor(descriptor, 12, nil)

// Solve for the font size whose monospace advance matches the cell width.
var lower = 4.0
var upper = 4.0 * cellWidth
var size = cellWidth
for _ in 0..<60 {
    size = (lower + upper) / 2
    let font = CTFontCreateWithFontDescriptor(descriptor, size, nil)
    var glyph = CTFontGetGlyphWithName(font, "M" as CFString)
    var advance = CGSize.zero
    CTFontGetAdvancesForGlyphs(font, .horizontal, &glyph, &advance, 1)
    if advance.width < cellWidth {
        lower = size
    } else {
        upper = size
    }
}
let font = CTFontCreateWithFontDescriptor(descriptor, size, nil)

let width = imageWidth
let height = Int(cellHeight.rounded())
let space = CGColorSpaceCreateDeviceRGB()
guard
    let context = CGContext(
        data: nil,
        width: width,
        height: height,
        bitsPerComponent: 8,
        bytesPerRow: 0,
        space: space,
        bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue
    )
else { fail("cannot create bitmap context") }

context.setFillColor(background)
context.fill(CGRect(x: 0, y: 0, width: Double(width), height: Double(height)))

let attributes: [CFString: Any] = [
    kCTFontAttributeName: font,
    kCTForegroundColorAttributeName: foreground,
]
let attributed = CFAttributedStringCreate(
    nil,
    text as CFString,
    attributes as CFDictionary
)!
let line = CTLineCreateWithAttributedString(attributed)
let ascent = CTFontGetAscent(font)
let descent = CTFontGetDescent(font)
let baseline = (cellHeight - (ascent + descent)) / 2 + descent
context.textPosition = CGPoint(x: 0, y: baseline)
CTLineDraw(line, context)

guard let image = context.makeImage() else { fail("cannot make image") }
let output = URL(fileURLWithPath: outputPath)
guard
    let destination = CGImageDestinationCreateWithURL(
        output as CFURL,
        UTType.png.identifier as CFString,
        1,
        nil
    )
else { fail("cannot create \(outputPath)") }
CGImageDestinationAddImage(destination, image, nil)
guard CGImageDestinationFinalize(destination) else { fail("cannot write \(outputPath)") }

// Report the solved metrics so the capture note can be checked.
print("""
    status-row: \(width)×\(height) font-size \(String(format: "%.3f", size)) \
    baseline \(String(format: "%.2f", baseline)) probe \(CTFontCopyPostScriptName(probe) as String)
    """)
