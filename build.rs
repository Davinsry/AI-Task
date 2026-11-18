use std::env;
use std::fs;
use std::path::PathBuf;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");

    let out_dir = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR not set"));
    let icon_64 = build_dib_icon(64);
    let icon_32 = build_dib_icon(32);
    let grp_icon = build_group_icon_dir(&[
        (64, icon_64.len() as u32, 1),
        (32, icon_32.len() as u32, 2),
    ]);
    let manifest = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<assembly xmlns="urn:schemas-microsoft-com:asm.v1" manifestVersion="1.0" xmlns:asmv3="urn:schemas-microsoft-com:asm.v3">
  <assemblyIdentity type="win32" name="Davin.AITask" version="0.1.0.0" processorArchitecture="amd64"/>
  <description>AI-Task - Stealth Screen Question Solver</description>
  <trustInfo xmlns="urn:schemas-microsoft-com:asm.v3">
    <security>
      <requestedPrivileges>
        <requestedExecutionLevel level="asInvoker" uiAccess="false"/>
      </requestedPrivileges>
    </security>
  </trustInfo>
  <compatibility xmlns="urn:schemas-microsoft-com:compatibility.v1">
    <application>
      <supportedOS Id="{8e0f7a12-bfb3-4fe8-b9a5-48fd50a15a9a}"/>
    </application>
  </compatibility>
  <asmv3:application>
    <asmv3:windowsSettings>
      <dpiAware xmlns="http://schemas.microsoft.com/SMI/2005/WindowsSettings">true/pm</dpiAware>
      <dpiAwareness xmlns="http://schemas.microsoft.com/SMI/2016/WindowsSettings">PerMonitorV2</dpiAwareness>
    </asmv3:windowsSettings>
  </asmv3:application>
</assembly>"#;

    let coff_bytes = build_rsrc_coff_x64(&icon_64, &icon_32, &grp_icon, manifest);
    let obj_path = out_dir.join("aitask_resources.o");
    fs::write(&obj_path, coff_bytes).expect("Failed to write COFF resource object");

    println!("cargo:rustc-link-arg-bins={}", obj_path.display());
}

fn build_dib_icon(size: usize) -> Vec<u8> {
    let xor_size = size * size * 4;
    let mask_row_bytes = ((size + 31) / 32) * 4;
    let and_size = mask_row_bytes * size;

    let mut out = Vec::with_capacity(40 + xor_size + and_size);
    push_u32(&mut out, 40);
    push_i32(&mut out, size as i32);
    push_i32(&mut out, (size * 2) as i32);
    push_u16(&mut out, 1);
    push_u16(&mut out, 32);
    push_u32(&mut out, 0);
    push_u32(&mut out, (xor_size + and_size) as u32);
    push_i32(&mut out, 0);
    push_i32(&mut out, 0);
    push_u32(&mut out, 0);
    push_u32(&mut out, 0);

    let center = (size as f32 - 1.0) * 0.5;
    let scale = size as f32 / 32.0;
    let half_box = 13.8 * scale;
    let corner_r = 6.5 * scale;

    for y_inv in 0..size {
        let y = size - 1 - y_inv;
        for x in 0..size {
            let fx = x as f32;
            let fy = y as f32;

            let dx = (fx - center).abs() - (half_box - corner_r);
            let dy = (fy - center).abs() - (half_box - corner_r);
            let outside_dist = (dx.max(0.0).powi(2) + dy.max(0.0).powi(2)).sqrt()
                + dx.max(dy).min(0.0)
                - corner_r;

            if outside_dist > 0.8 {
                out.extend_from_slice(&[0, 0, 0, 0]);
                continue;
            }

            let alpha = (0.8 - outside_dist).clamp(0.0, 1.0);
            let nx = (x as f32 + 0.5) / scale;
            let ny = (y as f32 + 0.5) / scale;

            let (r, g, b) = sample_aitask_logo_rgb(nx, ny, outside_dist / scale);

            let a_u8 = (alpha * 255.0) as u8;
            out.push(b);
            out.push(g);
            out.push(r);
            out.push(a_u8);
        }
    }

    out.resize(out.len() + and_size, 0);
    out
}

fn sdf_rrect(px: f32, py: f32, x1: f32, y1: f32, x2: f32, y2: f32, r: f32) -> f32 {
    let cx = (x1 + x2) * 0.5;
    let cy = (y1 + y2) * 0.5;
    let hx = (x2 - x1) * 0.5 - r;
    let hy = (y2 - y1) * 0.5 - r;
    let dx = (px - cx).abs() - hx;
    let dy = (py - cy).abs() - hy;
    (dx.max(0.0).powi(2) + dy.max(0.0).powi(2)).sqrt() + dx.max(dy).min(0.0) - r
}

fn sample_aitask_logo_rgb(nx: f32, ny: f32, outer_sd: f32) -> (u8, u8, u8) {
    let mut r = 17.0_f32;
    let mut g = 17.0_f32;
    let mut b = 17.0_f32;
    if outer_sd > -1.1 {
        r = 48.0;
        g = 48.0;
        b = 48.0;
    }

    let back_sd = sdf_rrect(nx, ny, 5.8, 5.6, 21.0, 20.8, 2.2);
    if back_sd < 0.5 {
        let cov = (0.5 - back_sd).clamp(0.0, 1.0);
        let (br, bg, bb) = if back_sd > -0.9 {
            (56.0, 189.0, 248.0)
        } else {
            (30.0, 41.0, 59.0)
        };
        r = r * (1.0 - cov) + br * cov;
        g = g * (1.0 - cov) + bg * cov;
        b = b * (1.0 - cov) + bb * cov;
    }

    let shadow_sd = sdf_rrect(nx, ny, 9.4, 9.4, 26.2, 26.2, 2.6);
    if shadow_sd < 1.2 {
        let scov = ((1.2 - shadow_sd) / 2.4).clamp(0.0, 0.65);
        r *= 1.0 - scov;
        g *= 1.0 - scov;
        b *= 1.0 - scov;
    }

    let front_sd = sdf_rrect(nx, ny, 10.2, 10.0, 26.0, 25.8, 2.3);
    if front_sd < 0.5 {
        let cov = (0.5 - front_sd).clamp(0.0, 1.0);
        let mut fr = 242.0_f32;
        let mut fg = 242.0_f32;
        let mut fb = 242.0_f32;

        // Emerald Green Answer Highlight Block (#22C55E)
        let green_sd = sdf_rrect(nx, ny, 12.2, 12.5, 24.0, 16.3, 0.85);
        if green_sd < 0.45 {
            let gcov = (0.45 - green_sd).clamp(0.0, 1.0);
            fr = fr * (1.0 - gcov) + 34.0 * gcov;
            fg = fg * (1.0 - gcov) + 197.0 * gcov;
            fb = fb * (1.0 - gcov) + 94.0 * gcov;

            let wbar_sd = sdf_rrect(nx, ny, 13.2, 13.8, 21.5, 15.0, 0.4);
            if wbar_sd < 0.4 {
                let wcov = (0.4 - wbar_sd).clamp(0.0, 1.0);
                fr = fr * (1.0 - wcov) + 255.0 * wcov;
                fg = fg * (1.0 - wcov) + 255.0 * wcov;
                fb = fb * (1.0 - wcov) + 255.0 * wcov;
            }
        }

        let bar1_sd = sdf_rrect(nx, ny, 12.2, 18.0, 23.0, 19.5, 0.45);
        if bar1_sd < 0.45 {
            let bcov = (0.45 - bar1_sd).clamp(0.0, 1.0);
            fr = fr * (1.0 - bcov) + 22.0 * bcov;
            fg = fg * (1.0 - bcov) + 22.0 * bcov;
            fb = fb * (1.0 - bcov) + 22.0 * bcov;
        }

        let bar2_sd = sdf_rrect(nx, ny, 12.2, 20.8, 19.4, 22.3, 0.45);
        if bar2_sd < 0.45 {
            let bcov = (0.45 - bar2_sd).clamp(0.0, 1.0);
            fr = fr * (1.0 - bcov) + 115.0 * bcov;
            fg = fg * (1.0 - bcov) + 115.0 * bcov;
            fb = fb * (1.0 - bcov) + 115.0 * bcov;
        }

        r = r * (1.0 - cov) + fr * cov;
        g = g * (1.0 - cov) + fg * cov;
        b = b * (1.0 - cov) + fb * cov;
    }

    (r as u8, g as u8, b as u8)
}

fn build_group_icon_dir(entries: &[(u8, u32, u16)]) -> Vec<u8> {
    let mut out = Vec::new();
    push_u16(&mut out, 0);
    push_u16(&mut out, 1);
    push_u16(&mut out, entries.len() as u16);

    for &(dim, bytes_in_res, id) in entries {
        out.push(dim);
        out.push(dim);
        out.push(0);
        out.push(0);
        push_u16(&mut out, 1);
        push_u16(&mut out, 32);
        push_u32(&mut out, bytes_in_res);
        push_u16(&mut out, id);
    }
    out
}

fn build_rsrc_coff_x64(
    icon_64: &[u8],
    icon_32: &[u8],
    grp_icon: &[u8],
    manifest: &[u8],
) -> Vec<u8> {
    let mut rsrc = Vec::new();

    push_dir_header(&mut rsrc, 0, 3);
    push_dir_entry(&mut rsrc, 3, true, 40);
    push_dir_entry(&mut rsrc, 14, true, 72);
    push_dir_entry(&mut rsrc, 24, true, 96);

    push_dir_header(&mut rsrc, 0, 2);
    push_dir_entry(&mut rsrc, 1, true, 120);
    push_dir_entry(&mut rsrc, 2, true, 144);

    push_dir_header(&mut rsrc, 0, 1);
    push_dir_entry(&mut rsrc, 1, true, 168);

    push_dir_header(&mut rsrc, 0, 1);
    push_dir_entry(&mut rsrc, 1, true, 192);

    push_dir_header(&mut rsrc, 0, 1);
    push_dir_entry(&mut rsrc, 0x0409, false, 216);

    push_dir_header(&mut rsrc, 0, 1);
    push_dir_entry(&mut rsrc, 0x0409, false, 232);

    push_dir_header(&mut rsrc, 0, 1);
    push_dir_entry(&mut rsrc, 0x0409, false, 248);

    push_dir_header(&mut rsrc, 0, 1);
    push_dir_entry(&mut rsrc, 0x0409, false, 264);

    assert_eq!(rsrc.len(), 216);

    let payloads: [&[u8]; 4] = [icon_64, icon_32, grp_icon, manifest];
    let mut payload_offsets = [0u32; 4];
    let mut cur_offset = 280u32;
    for (i, p) in payloads.iter().enumerate() {
        payload_offsets[i] = cur_offset;
        let aligned_len = ((p.len() + 7) & !7) as u32;
        cur_offset += aligned_len;
    }

    let reloc_addrs = [216u32, 232u32, 248u32, 264u32];
    for (i, p) in payloads.iter().enumerate() {
        push_u32(&mut rsrc, payload_offsets[i]);
        push_u32(&mut rsrc, p.len() as u32);
        push_u32(&mut rsrc, 0);
        push_u32(&mut rsrc, 0);
    }
    assert_eq!(rsrc.len(), 280);

    for p in &payloads {
        rsrc.extend_from_slice(p);
        while rsrc.len() % 8 != 0 {
            rsrc.push(0);
        }
    }

    let raw_data_ptr = 60u32;
    let reloc_ptr = raw_data_ptr + rsrc.len() as u32;
    let symtab_ptr = reloc_ptr + 40;

    let mut coff = Vec::new();
    push_u16(&mut coff, 0x8664);
    push_u16(&mut coff, 1);
    push_u32(&mut coff, 0);
    push_u32(&mut coff, symtab_ptr);
    push_u32(&mut coff, 1);
    push_u16(&mut coff, 0);
    push_u16(&mut coff, 0);

    coff.extend_from_slice(b".rsrc\0\0\0");
    push_u32(&mut coff, 0);
    push_u32(&mut coff, 0);
    push_u32(&mut coff, rsrc.len() as u32);
    push_u32(&mut coff, raw_data_ptr);
    push_u32(&mut coff, reloc_ptr);
    push_u32(&mut coff, 0);
    push_u16(&mut coff, 4);
    push_u16(&mut coff, 0);
    push_u32(&mut coff, 0x4000_0040);

    coff.extend_from_slice(&rsrc);

    for &addr in &reloc_addrs {
        push_u32(&mut coff, addr);
        push_u32(&mut coff, 0);
        push_u16(&mut coff, 0x0003);
    }

    coff.extend_from_slice(b".rsrc\0\0\0");
    push_u32(&mut coff, 0);
    push_i16(&mut coff, 1);
    push_u16(&mut coff, 0);
    coff.push(3);
    coff.push(0);

    push_u32(&mut coff, 4);

    coff
}

fn push_dir_header(buf: &mut Vec<u8>, named_entries: u16, id_entries: u16) {
    push_u32(buf, 0);
    push_u32(buf, 0);
    push_u16(buf, 0);
    push_u16(buf, 0);
    push_u16(buf, named_entries);
    push_u16(buf, id_entries);
}

fn push_dir_entry(buf: &mut Vec<u8>, id: u32, is_subdir: bool, offset: u32) {
    push_u32(buf, id);
    let off = if is_subdir {
        offset | 0x8000_0000
    } else {
        offset
    };
    push_u32(buf, off);
}

fn push_u16(buf: &mut Vec<u8>, v: u16) {
    buf.extend_from_slice(&v.to_le_bytes());
}
fn push_i16(buf: &mut Vec<u8>, v: i16) {
    buf.extend_from_slice(&v.to_le_bytes());
}
fn push_u32(buf: &mut Vec<u8>, v: u32) {
    buf.extend_from_slice(&v.to_le_bytes());
}
fn push_i32(buf: &mut Vec<u8>, v: i32) {
    buf.extend_from_slice(&v.to_le_bytes());
}
