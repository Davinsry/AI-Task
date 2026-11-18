use crate::selection::RectF;
use std::mem::size_of;
use windows::Graphics::Imaging::{
    BitmapAlphaMode, BitmapEncoder, BitmapPixelFormat, SoftwareBitmap,
};
use windows::Storage::Streams::{DataReader, DataWriter, InMemoryRandomAccessStream};
use windows::Win32::Foundation::{HWND, POINT, RECT};
use windows::Win32::Graphics::Gdi::{
    BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, DeleteDC, DeleteObject, GetDC, GetDIBits,
    GetMonitorInfoW, MonitorFromPoint, ReleaseDC, SelectObject, BITMAPINFO, BITMAPINFOHEADER,
    BI_RGB, CAPTUREBLT, DIB_RGB_COLORS, MONITORINFO, MONITOR_DEFAULTTONEAREST, SRCCOPY,
};

#[derive(Debug, Clone)]
pub struct CapturedFrame {
    pub monitor_x: i32,
    pub monitor_y: i32,
    pub width: u32,
    pub height: u32,
    pub local_click_x: f32,
    pub local_click_y: f32,
    pub pixels_bgra: Vec<u8>,
}

impl CapturedFrame {
    pub fn to_software_bitmap(&self) -> Result<SoftwareBitmap, String> {
        let writer = DataWriter::new().map_err(|e| format!("Gagal membuat DataWriter: {e}"))?;
        writer
            .WriteBytes(&self.pixels_bgra)
            .map_err(|e| format!("Gagal menulis byte bitmap ke stream buffer: {e}"))?;
        let buffer = writer
            .DetachBuffer()
            .map_err(|e| format!("Gagal mengambil IBuffer dari DataWriter: {e}"))?;

        SoftwareBitmap::CreateCopyWithAlphaFromBuffer(
            &buffer,
            BitmapPixelFormat::Bgra8,
            self.width as i32,
            self.height as i32,
            BitmapAlphaMode::Ignore,
        )
        .map_err(|e| format!("Gagal membuat SoftwareBitmap dari buffer: {e}"))
    }

    /// Crops a rectangular region from the captured monitor frame (adding a small 6px margin so edge glyphs are intact).
    /// If the cropped area exceeds `max_dim` (e.g. 1400px), it downscales cleanly to keep Vision API requests fast.
    pub fn crop_region(&self, rect: RectF, max_dim: u32) -> CapturedFrame {
        let pad = 6.0;
        let x0 = ((rect.x - pad).floor().max(0.0) as u32).min(self.width.saturating_sub(1));
        let y0 = ((rect.y - pad).floor().max(0.0) as u32).min(self.height.saturating_sub(1));
        let x1 = ((rect.right() + pad).ceil().max((x0 + 1) as f32) as u32).min(self.width);
        let y1 = ((rect.bottom() + pad).ceil().max((y0 + 1) as f32) as u32).min(self.height);

        let src_w = (x1 - x0).max(1);
        let src_h = (y1 - y0).max(1);

        let scale = if max_dim > 0 && (src_w > max_dim || src_h > max_dim) {
            (max_dim as f32 / (src_w.max(src_h) as f32)).clamp(0.15, 1.0)
        } else {
            1.0
        };

        let dst_w = ((src_w as f32 * scale).round() as u32).max(1);
        let dst_h = ((src_h as f32 * scale).round() as u32).max(1);
        let mut cropped = vec![255u8; (dst_w * dst_h * 4) as usize];

        for dy in 0..dst_h {
            let sy = y0 + ((dy as u64 * src_h as u64) / dst_h as u64) as u32;
            let sy = sy.min(self.height - 1);
            for dx in 0..dst_w {
                let sx = x0 + ((dx as u64 * src_w as u64) / dst_w as u64) as u32;
                let sx = sx.min(self.width - 1);
                let src_idx = ((sy * self.width + sx) * 4) as usize;
                let dst_idx = ((dy * dst_w + dx) * 4) as usize;
                if src_idx + 4 <= self.pixels_bgra.len() {
                    cropped[dst_idx..dst_idx + 4]
                        .copy_from_slice(&self.pixels_bgra[src_idx..src_idx + 4]);
                }
            }
        }

        CapturedFrame {
            monitor_x: self.monitor_x + x0 as i32,
            monitor_y: self.monitor_y + y0 as i32,
            width: dst_w,
            height: dst_h,
            local_click_x: 0.0,
            local_click_y: 0.0,
            pixels_bgra: cropped,
        }
    }

    /// Encodes the frame into compressed PNG bytes (using WinRT `BitmapEncoder` with pure-Rust fallback).
    pub fn to_png_bytes(&self) -> Result<Vec<u8>, String> {
        if let Ok(bytes) = self.encode_png_winrt() {
            if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
                return Ok(bytes);
            }
        }
        Ok(encode_png_pure_rust(
            self.width,
            self.height,
            &self.pixels_bgra,
        ))
    }

    /// Encodes the frame as a standard Base64 PNG string ready for Gemini / OpenAI Vision APIs.
    pub fn to_png_base64(&self) -> Result<String, String> {
        let png = self.to_png_bytes()?;
        Ok(base64_encode(&png))
    }

    fn encode_png_winrt(&self) -> Result<Vec<u8>, String> {
        let sb = self.to_software_bitmap()?;
        let stream = InMemoryRandomAccessStream::new()
            .map_err(|e| format!("Gagal membuat InMemoryRandomAccessStream: {e}"))?;
        let png_id =
            BitmapEncoder::PngEncoderId().map_err(|e| format!("Gagal membaca PngEncoderId: {e}"))?;
        let encoder = BitmapEncoder::CreateAsync(png_id, &stream)
            .map_err(|e| format!("Gagal memulai BitmapEncoder: {e}"))?
            .get()
            .map_err(|e| format!("Gagal membuat BitmapEncoder PNG: {e}"))?;

        encoder
            .SetSoftwareBitmap(&sb)
            .map_err(|e| format!("Gagal mengatur SoftwareBitmap ke encoder: {e}"))?;
        encoder
            .FlushAsync()
            .map_err(|e| format!("Gagal flush encoder PNG: {e}"))?
            .get()
            .map_err(|e| format!("Gagal menyelesaikan kompresi PNG: {e}"))?;

        let size = stream
            .Size()
            .map_err(|e| format!("Gagal membaca ukuran stream PNG: {e}"))? as u32;
        if size == 0 {
            return Err("Stream PNG kosong.".to_string());
        }

        let input = stream
            .GetInputStreamAt(0)
            .map_err(|e| format!("Gagal membuka input stream PNG: {e}"))?;
        let reader = DataReader::CreateDataReader(&input)
            .map_err(|e| format!("Gagal membuat DataReader PNG: {e}"))?;
        reader
            .LoadAsync(size)
            .map_err(|e| format!("Gagal memuat byte PNG: {e}"))?
            .get()
            .map_err(|e| format!("Gagal menunggu pembacaan byte PNG: {e}"))?;

        let mut buf = vec![0u8; size as usize];
        reader
            .ReadBytes(&mut buf)
            .map_err(|e| format!("Gagal menyalin byte PNG dari stream: {e}"))?;
        Ok(buf)
    }
}

/// Pure-Rust valid PNG encoder (RGB8 stored deflate blocks with Adler32 + CRC32) as an infallible fallback.
pub fn encode_png_pure_rust(width: u32, height: u32, pixels_bgra: &[u8]) -> Vec<u8> {
    let mut raw_scanlines = Vec::with_capacity(((width * 3 + 1) * height) as usize);
    for y in 0..height {
        raw_scanlines.push(0u8); // Filter type 0 (None)
        for x in 0..width {
            let idx = ((y * width + x) * 4) as usize;
            if idx + 3 <= pixels_bgra.len() {
                raw_scanlines.push(pixels_bgra[idx + 2]); // R
                raw_scanlines.push(pixels_bgra[idx + 1]); // G
                raw_scanlines.push(pixels_bgra[idx]); // B
            } else {
                raw_scanlines.extend_from_slice(&[0, 0, 0]);
            }
        }
    }

    // Build zlib stream with uncompressed DEFLATE blocks (RFC 1950 / 1951)
    let mut zlib = Vec::with_capacity(raw_scanlines.len() + (raw_scanlines.len() / 65535 + 2) * 5 + 6);
    zlib.push(0x78); // CMF
    zlib.push(0x01); // FLG (no preset dict, fastest)

    let chunks: Vec<&[u8]> = if raw_scanlines.is_empty() {
        vec![&[]]
    } else {
        raw_scanlines.chunks(65535).collect()
    };

    for (i, chunk) in chunks.iter().enumerate() {
        let is_last = i + 1 == chunks.len();
        zlib.push(if is_last { 0x01 } else { 0x00 });
        let len = chunk.len() as u16;
        let nlen = !len;
        zlib.extend_from_slice(&len.to_le_bytes());
        zlib.extend_from_slice(&nlen.to_le_bytes());
        zlib.extend_from_slice(chunk);
    }

    let adler = adler32(&raw_scanlines);
    zlib.extend_from_slice(&adler.to_be_bytes());

    let mut png = Vec::with_capacity(zlib.len() + 64);
    png.extend_from_slice(b"\x89PNG\r\n\x1a\n");

    let mut ihdr = Vec::with_capacity(13);
    ihdr.extend_from_slice(&width.to_be_bytes());
    ihdr.extend_from_slice(&height.to_be_bytes());
    ihdr.push(8); // bit depth 8
    ihdr.push(2); // color type 2 (Truecolor RGB)
    ihdr.push(0); // compression
    ihdr.push(0); // filter
    ihdr.push(0); // interlace
    write_png_chunk(&mut png, b"IHDR", &ihdr);
    write_png_chunk(&mut png, b"IDAT", &zlib);
    write_png_chunk(&mut png, b"IEND", &[]);
    png
}

fn write_png_chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    out.extend_from_slice(kind);
    out.extend_from_slice(data);
    let crc = crc32_png(kind, data);
    out.extend_from_slice(&crc.to_be_bytes());
}

fn adler32(data: &[u8]) -> u32 {
    let mut a: u32 = 1;
    let mut b: u32 = 0;
    for &byte in data {
        a = (a + byte as u32) % 65521;
        b = (b + a) % 65521;
    }
    (b << 16) | a
}

fn crc32_png(kind: &[u8; 4], data: &[u8]) -> u32 {
    let mut crc: u32 = 0xFFFF_FFFF;
    for &byte in kind.iter().chain(data.iter()) {
        crc ^= byte as u32;
        for _ in 0..8 {
            if (crc & 1) != 0 {
                crc = (crc >> 1) ^ 0xEDB8_8320;
            } else {
                crc >>= 1;
            }
        }
    }
    !crc
}

pub fn base64_encode(input: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = Vec::with_capacity(((input.len() + 2) / 3) * 4);

    for chunk in input.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = if chunk.len() > 1 { chunk[1] as u32 } else { 0 };
        let b2 = if chunk.len() > 2 { chunk[2] as u32 } else { 0 };
        let triple = (b0 << 16) | (b1 << 8) | b2;

        out.push(TABLE[((triple >> 18) & 0x3F) as usize]);
        out.push(TABLE[((triple >> 12) & 0x3F) as usize]);
        if chunk.len() > 1 {
            out.push(TABLE[((triple >> 6) & 0x3F) as usize]);
        } else {
            out.push(b'=');
        }
        if chunk.len() > 2 {
            out.push(TABLE[(triple & 0x3F) as usize]);
        } else {
            out.push(b'=');
        }
    }

    String::from_utf8(out).unwrap_or_default()
}

pub fn capture_monitor_at_point(screen_x: i32, screen_y: i32) -> Result<CapturedFrame, String> {
    unsafe {
        let pt = POINT {
            x: screen_x,
            y: screen_y,
        };
        let hmonitor = MonitorFromPoint(pt, MONITOR_DEFAULTTONEAREST);

        let mut mi = MONITORINFO {
            cbSize: size_of::<MONITORINFO>() as u32,
            rcMonitor: RECT::default(),
            rcWork: RECT::default(),
            dwFlags: 0,
        };

        if !GetMonitorInfoW(hmonitor, &mut mi).as_bool() {
            return Err("Gagal membaca informasi monitor aktif.".to_string());
        }

        let mon_x = mi.rcMonitor.left;
        let mon_y = mi.rcMonitor.top;
        let width = (mi.rcMonitor.right - mi.rcMonitor.left).max(1) as u32;
        let height = (mi.rcMonitor.bottom - mi.rcMonitor.top).max(1) as u32;

        let hdc_screen = GetDC(HWND::default());
        if hdc_screen.is_invalid() {
            return Err("Gagal mendapatkan DC layar utama.".to_string());
        }

        let hdc_mem = CreateCompatibleDC(hdc_screen);
        if hdc_mem.is_invalid() {
            let _ = ReleaseDC(HWND::default(), hdc_screen);
            return Err("Gagal membuat memory DC.".to_string());
        }

        let hbm = CreateCompatibleBitmap(hdc_screen, width as i32, height as i32);
        if hbm.is_invalid() {
            let _ = DeleteDC(hdc_mem);
            let _ = ReleaseDC(HWND::default(), hdc_screen);
            return Err("Gagal membuat compatible bitmap untuk tangkapan layar.".to_string());
        }

        let old_obj = SelectObject(hdc_mem, hbm);

        let blt_res = BitBlt(
            hdc_mem,
            0,
            0,
            width as i32,
            height as i32,
            hdc_screen,
            mon_x,
            mon_y,
            SRCCOPY | CAPTUREBLT,
        )
        .or_else(|_| {
            BitBlt(
                hdc_mem,
                0,
                0,
                width as i32,
                height as i32,
                hdc_screen,
                mon_x,
                mon_y,
                SRCCOPY,
            )
        });

        if let Err(e) = blt_res {
            let _ = SelectObject(hdc_mem, old_obj);
            let _ = DeleteObject(hbm);
            let _ = DeleteDC(hdc_mem);
            let _ = ReleaseDC(HWND::default(), hdc_screen);
            return Err(format!(
                "Gagal melakukan BitBlt tangkapan layar ({}x{} at {},{}): {e}",
                width, height, mon_x, mon_y
            ));
        }

        let mut bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: width as i32,
                biHeight: -(height as i32),
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                biSizeImage: width * height * 4,
                biXPelsPerMeter: 0,
                biYPelsPerMeter: 0,
                biClrUsed: 0,
                biClrImportant: 0,
            },
            bmiColors: Default::default(),
        };

        let mut pixels_bgra = vec![0u8; (width * height * 4) as usize];
        let lines = GetDIBits(
            hdc_mem,
            hbm,
            0,
            height,
            Some(pixels_bgra.as_mut_ptr() as *mut _),
            &mut bmi,
            DIB_RGB_COLORS,
        );

        let _ = SelectObject(hdc_mem, old_obj);
        let _ = DeleteObject(hbm);
        let _ = DeleteDC(hdc_mem);
        let _ = ReleaseDC(HWND::default(), hdc_screen);

        if lines == 0 {
            return Err("GetDIBits gagal mengekstrak piksel layar.".to_string());
        }

        for chunk in pixels_bgra.chunks_exact_mut(4) {
            chunk[3] = 255;
        }

        let local_click_x = ((screen_x - mon_x) as f32).clamp(0.0, width as f32);
        let local_click_y = ((screen_y - mon_y) as f32).clamp(0.0, height as f32);

        Ok(CapturedFrame {
            monitor_x: mon_x,
            monitor_y: mon_y,
            width,
            height,
            local_click_x,
            local_click_y,
            pixels_bgra,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_synthetic_frame_to_software_bitmap_and_png() {
        let frame = CapturedFrame {
            monitor_x: 0,
            monitor_y: 0,
            width: 64,
            height: 32,
            local_click_x: 10.0,
            local_click_y: 10.0,
            pixels_bgra: vec![255u8; 64 * 32 * 4],
        };
        let sb = frame
            .to_software_bitmap()
            .expect("SoftwareBitmap creation should succeed");
        assert_eq!(sb.PixelWidth().unwrap(), 64);
        assert_eq!(sb.PixelHeight().unwrap(), 32);

        let cropped = frame.crop_region(RectF::new(10.0, 5.0, 20.0, 15.0), 1200);
        assert!(cropped.width >= 20 && cropped.height >= 15);

        let png = cropped.to_png_bytes().expect("PNG encoding should succeed");
        assert!(png.starts_with(b"\x89PNG\r\n\x1a\n"));

        let b64 = cropped.to_png_base64().expect("Base64 PNG should succeed");
        assert!(b64.starts_with("iVBORw0KGgo"));
    }

    #[test]
    fn pure_rust_png_fallback_produces_valid_png_signature() {
        let png = encode_png_pure_rust(16, 16, &vec![128u8; 16 * 16 * 4]);
        assert!(png.starts_with(b"\x89PNG\r\n\x1a\n"));
        assert!(png.ends_with(b"IEND\xaeB`\x82"));
    }
}
