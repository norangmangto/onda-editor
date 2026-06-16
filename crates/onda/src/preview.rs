//! Rich content previews — inline images & PDF cards (Phase 6 W39).
//!
//! Two layers, both pure and unit-tested here; the editor wires them in:
//!
//! 1. **Capability detection** ([`detect_protocol`]) — which terminal graphics
//!    protocol is available (kitty graphics / iTerm2 / sixel / none).
//! 2. **Encoding & fallback** — turn image bytes into the right escape sequence
//!    ([`kitty_encode`]/[`iterm2_encode`]) for a supporting terminal, or build a
//!    plain-text **metadata card** ([`metadata_card`]) that works on any terminal
//!    (the graceful-degradation path).
//!
//! No new dependencies: base64 and image-header sniffing are hand-rolled (the
//! `image`/base64 crates aren't pre-approved, and kitty/iTerm2 decode the image
//! bytes themselves, so we never need to rasterize for those protocols).

use std::path::Path;

/// Terminal graphics protocol detected from the environment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GraphicsProtocol {
    /// Kitty graphics protocol (kitty, WezTerm, Ghostty, Konsole).
    Kitty,
    /// iTerm2 inline-image protocol (OSC 1337; iTerm2, WezTerm).
    Iterm2,
    /// DEC sixel (xterm -ti vt340, foot, mlterm, …). We detect it but render a
    /// card rather than encode sixel (would need a decoder + quantizer).
    Sixel,
    /// No inline-graphics support — use the metadata card.
    None,
}

/// What kind of non-text file we're previewing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PreviewKind {
    Image,
    Pdf,
}

/// Detect the best available graphics protocol from environment variables.
///
/// `get` looks up an env var (so this stays pure and testable). Order favours the
/// richer protocols; kitty wins over iTerm2 when a terminal supports both.
pub fn detect_protocol(get: impl Fn(&str) -> Option<String>) -> GraphicsProtocol {
    let term = get("TERM").unwrap_or_default();
    let term_program = get("TERM_PROGRAM").unwrap_or_default();

    // Kitty graphics: kitty sets KITTY_WINDOW_ID; Ghostty/WezTerm/Konsole advertise
    // via TERM/TERM_PROGRAM.
    if get("KITTY_WINDOW_ID").is_some()
        || term.contains("kitty")
        || term_program == "ghostty"
        || term_program == "WezTerm"
        || term.contains("ghostty")
    {
        return GraphicsProtocol::Kitty;
    }

    // iTerm2 inline images.
    if term_program == "iTerm.app" || get("ITERM_SESSION_ID").is_some() {
        return GraphicsProtocol::Iterm2;
    }

    // Sixel-capable terminals (best-effort sniff).
    if term.contains("sixel")
        || term == "foot"
        || term.contains("foot")
        || term.contains("mlterm")
        || term_program == "MacTerm"
    {
        return GraphicsProtocol::Sixel;
    }

    GraphicsProtocol::None
}

/// Classify a path as an image/PDF preview by extension, or `None` for text.
pub fn kind_for_path(path: &Path) -> Option<PreviewKind> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    match ext.as_str() {
        "png" | "jpg" | "jpeg" | "gif" | "bmp" | "webp" | "ico" => Some(PreviewKind::Image),
        "pdf" => Some(PreviewKind::Pdf),
        _ => None,
    }
}

/// Decoded image header info (format + pixel dimensions), best-effort.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImageInfo {
    pub format: &'static str,
    pub width: u32,
    pub height: u32,
}

/// Sniff common image formats' dimensions from the leading bytes — no decoding,
/// no dependency. Returns `None` if the header isn't recognised.
pub fn sniff_image(b: &[u8]) -> Option<ImageInfo> {
    // PNG: 8-byte signature, then IHDR with width/height as big-endian u32 at 16..24.
    if b.len() >= 24 && b[..8] == [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a] {
        let width = be_u32(&b[16..20]);
        let height = be_u32(&b[20..24]);
        return Some(ImageInfo {
            format: "PNG",
            width,
            height,
        });
    }
    // GIF: "GIF87a"/"GIF89a", then width/height as little-endian u16 at 6..10.
    if b.len() >= 10 && (b.starts_with(b"GIF87a") || b.starts_with(b"GIF89a")) {
        let width = le_u16(&b[6..8]) as u32;
        let height = le_u16(&b[8..10]) as u32;
        return Some(ImageInfo {
            format: "GIF",
            width,
            height,
        });
    }
    // BMP: "BM", then width/height as little-endian i32 at 18..26.
    if b.len() >= 26 && b.starts_with(b"BM") {
        let width = le_u32(&b[18..22]);
        let height = le_u32(&b[22..26]);
        return Some(ImageInfo {
            format: "BMP",
            width,
            height,
        });
    }
    // WebP: "RIFF"...."WEBP"; VP8X/VP8L/VP8 carry the dimensions.
    if b.len() >= 30 && b.starts_with(b"RIFF") && &b[8..12] == b"WEBP" {
        if let Some(info) = sniff_webp(b) {
            return Some(info);
        }
    }
    // JPEG: walk segment markers to the SOF (start-of-frame) for dimensions.
    if b.len() >= 4 && b[0] == 0xFF && b[1] == 0xD8 {
        if let Some((w, h)) = jpeg_dimensions(b) {
            return Some(ImageInfo {
                format: "JPEG",
                width: w,
                height: h,
            });
        }
        return Some(ImageInfo {
            format: "JPEG",
            width: 0,
            height: 0,
        });
    }
    None
}

fn be_u32(b: &[u8]) -> u32 {
    ((b[0] as u32) << 24) | ((b[1] as u32) << 16) | ((b[2] as u32) << 8) | b[3] as u32
}
fn le_u32(b: &[u8]) -> u32 {
    (b[0] as u32) | ((b[1] as u32) << 8) | ((b[2] as u32) << 16) | ((b[3] as u32) << 24)
}
fn le_u16(b: &[u8]) -> u16 {
    (b[0] as u16) | ((b[1] as u16) << 8)
}

fn sniff_webp(b: &[u8]) -> Option<ImageInfo> {
    // Chunk FourCC at offset 12.
    match &b[12..16] {
        b"VP8X" => {
            // 24-bit width-1 / height-1 (little-endian) at 24..30.
            if b.len() < 30 {
                return None;
            }
            let w = 1 + ((b[24] as u32) | ((b[25] as u32) << 8) | ((b[26] as u32) << 16));
            let h = 1 + ((b[27] as u32) | ((b[28] as u32) << 8) | ((b[29] as u32) << 16));
            Some(ImageInfo {
                format: "WebP",
                width: w,
                height: h,
            })
        }
        b"VP8 " => {
            // Lossy: 14-bit dims at 26.. after the 0x9d012a start code.
            if b.len() < 30 {
                return None;
            }
            let w = (le_u16(&b[26..28]) & 0x3fff) as u32;
            let h = (le_u16(&b[28..30]) & 0x3fff) as u32;
            Some(ImageInfo {
                format: "WebP",
                width: w,
                height: h,
            })
        }
        b"VP8L" => {
            // Lossless: 14-bit width-1/height-1 packed after the 0x2f signature.
            if b.len() < 25 {
                return None;
            }
            let bits = (b[21] as u32)
                | ((b[22] as u32) << 8)
                | ((b[23] as u32) << 16)
                | ((b[24] as u32) << 24);
            let w = (bits & 0x3fff) + 1;
            let h = ((bits >> 14) & 0x3fff) + 1;
            Some(ImageInfo {
                format: "WebP",
                width: w,
                height: h,
            })
        }
        _ => Some(ImageInfo {
            format: "WebP",
            width: 0,
            height: 0,
        }),
    }
}

/// Walk JPEG segments to the first SOF marker and read its dimensions.
fn jpeg_dimensions(b: &[u8]) -> Option<(u32, u32)> {
    let mut i = 2; // skip SOI (FFD8)
    while i + 9 < b.len() {
        if b[i] != 0xFF {
            i += 1;
            continue;
        }
        let marker = b[i + 1];
        // SOF0..SOF15 except DHT(C4)/JPG(C8)/DAC(CC) carry frame dimensions.
        if (0xC0..=0xCF).contains(&marker) && marker != 0xC4 && marker != 0xC8 && marker != 0xCC {
            // height at i+5..7, width at i+7..9 (big-endian u16).
            let h = ((b[i + 5] as u32) << 8) | b[i + 6] as u32;
            let w = ((b[i + 7] as u32) << 8) | b[i + 8] as u32;
            return Some((w, h));
        }
        // Otherwise skip this segment by its length field.
        let len = ((b[i + 2] as usize) << 8) | b[i + 3] as usize;
        if len < 2 {
            return None;
        }
        i += 2 + len;
    }
    None
}

/// Best-effort PDF page count: count `/Type /Page` objects (ignoring `/Pages`).
/// Approximate but dependency-free; used only for the metadata card.
pub fn pdf_page_count(b: &[u8]) -> Option<usize> {
    let hay = b;
    let needle = b"/Type";
    let mut count = 0usize;
    let mut i = 0;
    while i + needle.len() < hay.len() {
        if &hay[i..i + needle.len()] == needle {
            // Skip whitespace then expect "/Page" but not "/Pages".
            let mut j = i + needle.len();
            while j < hay.len() && (hay[j] == b' ' || hay[j] == b'\r' || hay[j] == b'\n') {
                j += 1;
            }
            if hay[j..].starts_with(b"/Page") {
                let after = j + b"/Page".len();
                let is_pages = hay.get(after) == Some(&b's');
                if !is_pages {
                    count += 1;
                }
            }
        }
        i += 1;
    }
    if count > 0 {
        Some(count)
    } else {
        None
    }
}

/// Human-readable byte size, e.g. `1.4 MB`.
pub fn human_size(bytes: usize) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut v = bytes as f64;
    let mut u = 0;
    while v >= 1024.0 && u < UNITS.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    if u == 0 {
        format!("{bytes} {}", UNITS[0])
    } else {
        format!("{v:.1} {}", UNITS[u])
    }
}

/// Build the plain-text metadata card shown when inline graphics aren't available
/// (or for PDFs, which we don't rasterize). Pure — the editor renders these lines.
pub fn metadata_card(
    path: &Path,
    kind: PreviewKind,
    info: Option<ImageInfo>,
    pages: Option<usize>,
    byte_len: usize,
) -> Vec<String> {
    let name = path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("<file>");
    let mut out = Vec::new();
    let icon = match kind {
        PreviewKind::Image => "🖼",
        PreviewKind::Pdf => "📄",
    };
    out.push(format!("  {icon}  {name}"));
    out.push(String::new());
    match kind {
        PreviewKind::Image => {
            let fmt = info.map(|i| i.format).unwrap_or("image");
            out.push(format!("  format    {fmt}"));
            if let Some(i) = info {
                if i.width > 0 && i.height > 0 {
                    out.push(format!("  size      {} × {} px", i.width, i.height));
                }
            }
        }
        PreviewKind::Pdf => {
            out.push("  format    PDF".to_string());
            if let Some(p) = pages {
                out.push(format!("  pages     {p}"));
            }
        }
    }
    out.push(format!("  on disk   {}", human_size(byte_len)));
    out.push(String::new());
    out.push("  (preview — this buffer is read-only)".to_string());
    out
}

// ── base64 (standard alphabet, padded) ───────────────────────────────────────

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// Standard-alphabet base64 with `=` padding (RFC 4648).
pub fn base64(data: &[u8]) -> String {
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = *chunk.get(1).unwrap_or(&0) as u32;
        let b2 = *chunk.get(2).unwrap_or(&0) as u32;
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(B64[((n >> 18) & 63) as usize] as char);
        out.push(B64[((n >> 12) & 63) as usize] as char);
        out.push(if chunk.len() > 1 {
            B64[((n >> 6) & 63) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            B64[(n & 63) as usize] as char
        } else {
            '='
        });
    }
    out
}

/// Encode an image for the **kitty graphics protocol**, sized to fit `cols`×`rows`
/// terminal cells. The payload is chunked (kitty requires ≤4096 base64 bytes per
/// escape) with `m=1` continuation on all but the last chunk.
pub fn kitty_encode(bytes: &[u8], cols: u16, rows: u16) -> String {
    let b64 = base64(bytes);
    let chunks: Vec<&str> = chunk_str(&b64, 4096);
    let mut out = String::new();
    for (i, ch) in chunks.iter().enumerate() {
        let last = i + 1 == chunks.len();
        out.push_str("\x1b_G");
        if i == 0 {
            // a=T transmit+display, f=100 = PNG/auto-detected, c/r = cell box.
            out.push_str(&format!("a=T,f=100,c={cols},r={rows},"));
        }
        out.push_str(&format!("m={};", if last { 0 } else { 1 }));
        out.push_str(ch);
        out.push_str("\x1b\\");
    }
    out
}

/// Encode an image for the **iTerm2 inline-image protocol** (OSC 1337).
pub fn iterm2_encode(bytes: &[u8], name: &str, cols: u16, rows: u16) -> String {
    let b64 = base64(bytes);
    let name_b64 = base64(name.as_bytes());
    format!(
        "\x1b]1337;File=name={};size={};width={};height={};inline=1;preserveAspectRatio=1:{}\x07",
        name_b64,
        bytes.len(),
        cols,
        rows,
        b64,
    )
}

fn chunk_str(s: &str, n: usize) -> Vec<&str> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < s.len() {
        let end = (i + n).min(s.len());
        out.push(&s[i..end]);
        i = end;
    }
    if out.is_empty() {
        out.push("");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn p(s: &str) -> PathBuf {
        PathBuf::from(s)
    }

    #[test]
    fn detects_kitty_from_window_id() {
        let proto = detect_protocol(|k| (k == "KITTY_WINDOW_ID").then(|| "1".to_string()));
        assert_eq!(proto, GraphicsProtocol::Kitty);
    }

    #[test]
    fn detects_iterm_and_sixel_and_none() {
        let iterm = detect_protocol(|k| (k == "TERM_PROGRAM").then(|| "iTerm.app".to_string()));
        assert_eq!(iterm, GraphicsProtocol::Iterm2);
        let sixel = detect_protocol(|k| (k == "TERM").then(|| "foot".to_string()));
        assert_eq!(sixel, GraphicsProtocol::Sixel);
        let none = detect_protocol(|k| (k == "TERM").then(|| "xterm-256color".to_string()));
        assert_eq!(none, GraphicsProtocol::None);
    }

    #[test]
    fn ghostty_is_kitty() {
        let proto = detect_protocol(|k| (k == "TERM_PROGRAM").then(|| "ghostty".to_string()));
        assert_eq!(proto, GraphicsProtocol::Kitty);
    }

    #[test]
    fn classifies_paths() {
        assert_eq!(kind_for_path(&p("a.PNG")), Some(PreviewKind::Image));
        assert_eq!(kind_for_path(&p("a.jpeg")), Some(PreviewKind::Image));
        assert_eq!(kind_for_path(&p("doc.pdf")), Some(PreviewKind::Pdf));
        assert_eq!(kind_for_path(&p("main.rs")), None);
        assert_eq!(kind_for_path(&p("noext")), None);
    }

    #[test]
    fn sniffs_png_dimensions() {
        let mut b = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
        b.extend_from_slice(&[0, 0, 0, 13]); // IHDR length
        b.extend_from_slice(b"IHDR");
        b.extend_from_slice(&640u32.to_be_bytes());
        b.extend_from_slice(&480u32.to_be_bytes());
        let info = sniff_image(&b).unwrap();
        assert_eq!(info.format, "PNG");
        assert_eq!((info.width, info.height), (640, 480));
    }

    #[test]
    fn sniffs_gif_and_bmp() {
        let mut gif = b"GIF89a".to_vec();
        gif.extend_from_slice(&320u16.to_le_bytes());
        gif.extend_from_slice(&200u16.to_le_bytes());
        let g = sniff_image(&gif).unwrap();
        assert_eq!((g.format, g.width, g.height), ("GIF", 320, 200));

        let mut bmp = b"BM".to_vec();
        bmp.resize(18, 0);
        bmp.extend_from_slice(&100i32.to_le_bytes());
        bmp.extend_from_slice(&50i32.to_le_bytes());
        let m = sniff_image(&bmp).unwrap();
        assert_eq!((m.format, m.width, m.height), ("BMP", 100, 50));
    }

    #[test]
    fn sniffs_jpeg_sof() {
        // SOI, then a SOF0 segment: FFC0, len(0011), prec, H(0x00F0=240), W(0x0140=320)
        let b = vec![
            0xFF, 0xD8, // SOI
            0xFF, 0xC0, 0x00, 0x11, 0x08, 0x00, 0xF0, 0x01, 0x40, 0x03,
        ];
        let info = sniff_image(&b).unwrap();
        assert_eq!(info.format, "JPEG");
        assert_eq!((info.width, info.height), (320, 240));
    }

    #[test]
    fn unknown_bytes_sniff_none() {
        assert!(sniff_image(b"not an image").is_none());
        assert!(sniff_image(&[]).is_none());
    }

    #[test]
    fn pdf_counts_pages() {
        let pdf = b"%PDF-1.7\n/Type /Pages /Count 2\n/Type /Page x\n/Type/Page y\n";
        assert_eq!(pdf_page_count(pdf), Some(2));
        assert_eq!(pdf_page_count(b"no pages here"), None);
    }

    #[test]
    fn human_sizes() {
        assert_eq!(human_size(512), "512 B");
        assert_eq!(human_size(1536), "1.5 KB");
        assert_eq!(human_size(5 * 1024 * 1024), "5.0 MB");
    }

    #[test]
    fn base64_known_vectors() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foob"), "Zm9vYg==");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn kitty_wraps_and_chunks() {
        let big = vec![0u8; 5000]; // base64 → ~6668 chars → 2 chunks
        let s = kitty_encode(&big, 40, 20);
        assert!(s.starts_with("\x1b_Ga=T,f=100,c=40,r=20,"));
        assert!(s.ends_with("\x1b\\"));
        assert!(s.contains("m=1;")); // a continuation chunk exists
        assert!(s.contains("m=0;")); // and a final chunk
    }

    #[test]
    fn iterm_encode_structure() {
        let s = iterm2_encode(b"\x89PNG", "a.png", 10, 5);
        assert!(s.starts_with("\x1b]1337;File=name="));
        assert!(s.contains("width=10;height=5;"));
        assert!(s.contains("inline=1"));
        assert!(s.ends_with('\x07'));
    }

    #[test]
    fn card_has_name_format_and_size() {
        let info = Some(ImageInfo {
            format: "PNG",
            width: 800,
            height: 600,
        });
        let lines = metadata_card(&p("/tmp/cat.png"), PreviewKind::Image, info, None, 2048);
        let joined = lines.join("\n");
        assert!(joined.contains("cat.png"));
        assert!(joined.contains("PNG"));
        assert!(joined.contains("800 × 600"));
        assert!(joined.contains("2.0 KB"));
    }

    #[test]
    fn pdf_card_shows_pages() {
        let lines = metadata_card(&p("doc.pdf"), PreviewKind::Pdf, None, Some(12), 4096);
        let joined = lines.join("\n");
        assert!(joined.contains("PDF"));
        assert!(joined.contains("pages"));
        assert!(joined.contains("12"));
    }
}
