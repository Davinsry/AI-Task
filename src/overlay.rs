use crate::ai::{solve_question_hybrid, AiAnswer, AiConfig};
use crate::capture::CapturedFrame;
use crate::clipboard::copy_text_to_clipboard;
use crate::hook::{
    set_overlay_fullscreen_capturing, set_overlay_hwnd, WM_APP_OVERLAY_COPY,
};
use crate::ocr::recognize_frame;
use crate::selection::{OcrLayout, RectF, SelectionMode, SelectionResult};
use std::ffi::c_void;
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::Instant;
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::Graphics::Direct2D::Common::{
    D2D1_ALPHA_MODE_IGNORE, D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_COLOR_F, D2D1_GRADIENT_STOP,
    D2D1_PIXEL_FORMAT, D2D_POINT_2F, D2D_RECT_F, D2D_SIZE_U,
};
use windows::Win32::Graphics::Direct2D::{
    D2D1CreateFactory, ID2D1Bitmap, ID2D1Factory, ID2D1HwndRenderTarget,
    ID2D1LinearGradientBrush, ID2D1RadialGradientBrush, ID2D1SolidColorBrush,
    D2D1_BITMAP_PROPERTIES, D2D1_DRAW_TEXT_OPTIONS_CLIP, D2D1_DRAW_TEXT_OPTIONS_NONE,
    D2D1_ELLIPSE, D2D1_EXTEND_MODE_CLAMP, D2D1_FACTORY_TYPE_SINGLE_THREADED, D2D1_GAMMA_2_2,
    D2D1_HWND_RENDER_TARGET_PROPERTIES, D2D1_LINEAR_GRADIENT_BRUSH_PROPERTIES,
    D2D1_PRESENT_OPTIONS_NONE, D2D1_RADIAL_GRADIENT_BRUSH_PROPERTIES,
    D2D1_RENDER_TARGET_PROPERTIES, D2D1_RENDER_TARGET_TYPE_DEFAULT,
    D2D1_RENDER_TARGET_USAGE_NONE, D2D1_ROUNDED_RECT,
};
use windows::Win32::Graphics::DirectWrite::{
    DWriteCreateFactory, IDWriteFactory, IDWriteTextFormat, DWRITE_FACTORY_TYPE_SHARED,
    DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_WEIGHT_BOLD,
    DWRITE_FONT_WEIGHT_NORMAL, DWRITE_FONT_WEIGHT_SEMI_BOLD, DWRITE_MEASURING_MODE_NATURAL,
    DWRITE_PARAGRAPH_ALIGNMENT_CENTER, DWRITE_PARAGRAPH_ALIGNMENT_NEAR,
    DWRITE_TEXT_ALIGNMENT_CENTER, DWRITE_TEXT_ALIGNMENT_LEADING, DWRITE_TEXT_METRICS,
    DWRITE_WORD_WRAPPING_WRAP,
};
use windows::Win32::Graphics::Dwm::{
    DwmSetWindowAttribute, DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND,
};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::Graphics::Gdi::{BeginPaint, EndPaint, InvalidateRect, PAINTSTRUCT};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, ReleaseCapture, SetCapture, VK_CONTROL, VK_ESCAPE, VK_MENU, VK_SHIFT,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetCursorPos, GetMessageW,
    HWND_TOPMOST, IsWindow, KillTimer, LoadCursorW, RegisterClassW, SetCursor, SetTimer,
    SetWindowPos, ShowWindow, TranslateMessage, CS_HREDRAW, CS_VREDRAW, IDC_ARROW, IDC_CROSS,
    IDC_HAND, IDC_IBEAM, IDC_SIZEALL, MA_NOACTIVATE, MSG, SWP_NOACTIVATE, SW_SHOWNOACTIVATE,
    WM_CLOSE, WM_DESTROY, WM_ERASEBKGND, WM_KEYDOWN, WM_LBUTTONDOWN, WM_LBUTTONUP,
    WM_MOUSEACTIVATE, WM_MOUSEMOVE, WM_MOUSEWHEEL, WM_PAINT, WM_RBUTTONUP, WM_SETCURSOR,
    WM_TIMER, WNDCLASSW, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
};

const ANIM_TIMER_ID: usize = 101;
const CLOSE_DELAY_TIMER_ID: usize = 102;
const ANIM_INTERVAL_MS: u32 = 16;

const HUD_WIDTH: u32 = 480;
const HUD_HEIGHT: u32 = 350;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OverlayPhase {
    /// Phase 1: Full-monitor screen freeze where the user drags a box or linear text over the question.
    Selecting,
    /// Phase 2: Compact floating stealth HUD card (`WS_EX_NOACTIVATE`) showing the AI answer next to the question.
    Answering,
}

struct CachedBrushes {
    solid: ID2D1SolidColorBrush,
    orb_purple: ID2D1RadialGradientBrush,
    orb_pink: ID2D1RadialGradientBrush,
    orb_blue: ID2D1RadialGradientBrush,
    edge_grad: ID2D1LinearGradientBrush,
}

struct OverlayState {
    hwnd: HWND,
    frame: CapturedFrame,
    d2d_factory: ID2D1Factory,
    dwrite_factory: IDWriteFactory,
    text_format_ui: IDWriteTextFormat,
    text_format_small: IDWriteTextFormat,
    text_format_hud_title: IDWriteTextFormat,
    text_format_hud_answer_big: IDWriteTextFormat,
    text_format_hud_body: IDWriteTextFormat,
    text_format_hud_small_left: IDWriteTextFormat,
    render_target: Option<ID2D1HwndRenderTarget>,
    screen_bitmap: Option<ID2D1Bitmap>,
    brushes: Option<CachedBrushes>,

    phase: OverlayPhase,
    start_instant: Instant,
    ocr_rx: Option<Receiver<Result<OcrLayout, String>>>,
    ocr_layout: OcrLayout,
    line_capsules: Vec<RectF>,
    ocr_ready_instant: Option<Instant>,
    ocr_loading: bool,
    ocr_error: Option<String>,

    selection_mode: SelectionMode,
    default_mode: SelectionMode,
    is_dragging: bool,
    drag_start: (f32, f32),
    drag_current: (f32, f32),
    hover_pos: (f32, f32),
    glow_pos: (f32, f32),
    current_selection: SelectionResult,

    // Phase 2 (Floating Stealth Answer HUD) state
    ai_rx: Option<Receiver<Result<AiAnswer, String>>>,
    ai_loading: bool,
    ai_start_instant: Option<Instant>,
    ai_answer: Option<AiAnswer>,
    ai_error: Option<String>,
    captured_ocr_text: String,
    hud_scroll_y: f32,
    hud_max_scroll_y: f32,
    hud_dragging_window: bool,
    hud_drag_offset: (i32, i32),

    status_toast: Option<(String, Instant)>,
    closing_soon: bool,
}

fn compute_line_capsules(layout: &OcrLayout) -> Vec<RectF> {
    let mut capsules = Vec::new();
    for line in &layout.lines {
        let mut current: Option<RectF> = None;
        for word in &line.words {
            let wb = word.bounds;
            match current.as_mut() {
                None => current = Some(wb),
                Some(acc) => {
                    let min_h = acc.height.min(wb.height).max(8.0);
                    let max_gap = (min_h * 0.90).clamp(4.0, 18.0);
                    let same_baseline = (acc.center_y() - wb.center_y()).abs() <= min_h * 0.55;
                    if same_baseline && (wb.x - acc.right()) <= max_gap {
                        let min_x = acc.x.min(wb.x);
                        let min_y = acc.y.min(wb.y);
                        let max_x = acc.right().max(wb.right());
                        let max_y = acc.bottom().max(wb.bottom());
                        *acc = RectF::new(min_x, min_y, max_x - min_x, max_y - min_y);
                    } else {
                        capsules.push(*acc);
                        *acc = wb;
                    }
                }
            }
        }
        if let Some(last) = current {
            capsules.push(last);
        }
    }
    capsules
}

impl OverlayState {
    fn width_f(&self) -> f32 {
        match self.phase {
            OverlayPhase::Selecting => self.frame.width as f32,
            OverlayPhase::Answering => HUD_WIDTH as f32,
        }
    }

    fn height_f(&self) -> f32 {
        match self.phase {
            OverlayPhase::Selecting => self.frame.height as f32,
            OverlayPhase::Answering => HUD_HEIGHT as f32,
        }
    }

    fn top_toolbar_rect(&self) -> RectF {
        let w = 528.0_f32.min(self.width_f() - 32.0);
        let h = 44.0;
        let x = (self.width_f() - w) * 0.5;
        let y = 22.0;
        RectF::new(x, y, w, h)
    }

    fn btn_box_rect(&self) -> RectF {
        let bar = self.top_toolbar_rect();
        RectF::new(bar.x + 8.0, bar.y + 6.0, 114.0, 32.0)
    }

    fn btn_linear_rect(&self) -> RectF {
        let bar = self.top_toolbar_rect();
        RectF::new(bar.x + 128.0, bar.y + 6.0, 114.0, 32.0)
    }

    fn btn_solve_all_rect(&self) -> RectF {
        let bar = self.top_toolbar_rect();
        RectF::new(bar.x + 248.0, bar.y + 6.0, 118.0, 32.0)
    }

    fn btn_copy_all_rect(&self) -> RectF {
        let bar = self.top_toolbar_rect();
        RectF::new(bar.x + 372.0, bar.y + 6.0, 108.0, 32.0)
    }

    fn btn_close_rect(&self) -> RectF {
        let bar = self.top_toolbar_rect();
        RectF::new(bar.right() - 40.0, bar.y + 6.0, 32.0, 32.0)
    }

    fn status_pill_rect(&self) -> RectF {
        let w = 540.0_f32.min(self.width_f() - 32.0);
        let h = 36.0;
        let x = (self.width_f() - w) * 0.5;
        let y = 72.0;
        RectF::new(x, y, w, h)
    }

    // HUD control rectangles (Phase 2: `HUD_WIDTH x HUD_HEIGHT` = `460 x 310`)
    fn hud_header_rect(&self) -> RectF {
        RectF::new(0.0, 0.0, HUD_WIDTH as f32, 42.0)
    }

    fn hud_close_btn_rect(&self) -> RectF {
        RectF::new(HUD_WIDTH as f32 - 38.0, 7.0, 28.0, 28.0)
    }

    fn hud_btn_copy_answer_rect(&self) -> RectF {
        RectF::new(16.0, HUD_HEIGHT as f32 - 44.0, 156.0, 32.0)
    }

    fn hud_btn_copy_ocr_rect(&self) -> RectF {
        RectF::new(180.0, HUD_HEIGHT as f32 - 44.0, 140.0, 32.0)
    }

    fn hud_btn_scroll_up_rect(&self) -> RectF {
        RectF::new(328.0, HUD_HEIGHT as f32 - 44.0, 36.0, 32.0)
    }

    fn hud_btn_scroll_down_rect(&self) -> RectF {
        RectF::new(368.0, HUD_HEIGHT as f32 - 44.0, 36.0, 32.0)
    }

    fn hud_btn_dismiss_rect(&self) -> RectF {
        RectF::new(412.0, HUD_HEIGHT as f32 - 44.0, 52.0, 32.0)
    }

    fn is_point_on_toolbar(&self, x: f32, y: f32) -> bool {
        self.top_toolbar_rect().contains(x, y)
    }

    fn needs_animation_tick(&self) -> bool {
        if self.phase == OverlayPhase::Answering {
            return self.ai_loading || self.status_toast.is_some();
        }
        if self.ocr_loading {
            return true;
        }
        if self.start_instant.elapsed().as_secs_f32() < 0.45 {
            return true;
        }
        if let Some(t) = self.ocr_ready_instant {
            if t.elapsed().as_secs_f32() < 0.70 {
                return true;
            }
        }
        let dx = (self.hover_pos.0 - self.glow_pos.0).abs();
        let dy = (self.hover_pos.1 - self.glow_pos.1).abs();
        dx > 0.6 || dy > 0.6
    }

    fn ensure_render_target(&mut self) -> Result<(), String> {
        if self.render_target.is_some() {
            return Ok(());
        }

        let rt_w = self.width_f() as u32;
        let rt_h = self.height_f() as u32;

        unsafe {
            let render_props = D2D1_RENDER_TARGET_PROPERTIES {
                r#type: D2D1_RENDER_TARGET_TYPE_DEFAULT,
                pixelFormat: D2D1_PIXEL_FORMAT {
                    format: DXGI_FORMAT_B8G8R8A8_UNORM,
                    alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
                },
                dpiX: 96.0,
                dpiY: 96.0,
                usage: D2D1_RENDER_TARGET_USAGE_NONE,
                minLevel: Default::default(),
            };

            let hwnd_props = D2D1_HWND_RENDER_TARGET_PROPERTIES {
                hwnd: self.hwnd,
                pixelSize: D2D_SIZE_U {
                    width: rt_w,
                    height: rt_h,
                },
                presentOptions: D2D1_PRESENT_OPTIONS_NONE,
            };

            let rt = self
                .d2d_factory
                .CreateHwndRenderTarget(&render_props, &hwnd_props)
                .map_err(|e| format!("Gagal membuat Direct2D HwndRenderTarget: {e}"))?;

            if self.phase == OverlayPhase::Selecting && !self.frame.pixels_bgra.is_empty() {
                let bmp_props = D2D1_BITMAP_PROPERTIES {
                    pixelFormat: D2D1_PIXEL_FORMAT {
                        format: DXGI_FORMAT_B8G8R8A8_UNORM,
                        alphaMode: D2D1_ALPHA_MODE_IGNORE,
                    },
                    dpiX: 96.0,
                    dpiY: 96.0,
                };

                let bmp = rt
                    .CreateBitmap(
                        D2D_SIZE_U {
                            width: self.frame.width,
                            height: self.frame.height,
                        },
                        Some(self.frame.pixels_bgra.as_ptr() as *const _),
                        self.frame.width * 4,
                        &bmp_props,
                    )
                    .map_err(|e| format!("Gagal membuat Direct2D Bitmap layar: {e}"))?;
                self.screen_bitmap = Some(bmp);
            }

            let solid = rt
                .CreateSolidColorBrush(
                    &D2D1_COLOR_F {
                        r: 1.0,
                        g: 1.0,
                        b: 1.0,
                        a: 1.0,
                    },
                    None,
                )
                .map_err(|e| format!("Gagal membuat SolidColorBrush: {e}"))?;

            let orb_purple = create_cached_radial_brush(
                &rt,
                (0.68, 0.22, 0.98, 0.36),
                (0.38, 0.12, 0.78, 0.14),
            )?;
            let orb_pink = create_cached_radial_brush(
                &rt,
                (0.96, 0.38, 0.62, 0.30),
                (0.68, 0.20, 0.88, 0.11),
            )?;
            let orb_blue = create_cached_radial_brush(
                &rt,
                (0.16, 0.68, 0.98, 0.28),
                (0.32, 0.28, 0.92, 0.10),
            )?;

            let border_stops = [
                D2D1_GRADIENT_STOP {
                    position: 0.0,
                    color: D2D1_COLOR_F {
                        r: 0.18,
                        g: 0.62,
                        b: 0.98,
                        a: 0.85,
                    },
                },
                D2D1_GRADIENT_STOP {
                    position: 0.5,
                    color: D2D1_COLOR_F {
                        r: 0.22,
                        g: 0.78,
                        b: 0.54,
                        a: 0.85,
                    },
                },
                D2D1_GRADIENT_STOP {
                    position: 1.0,
                    color: D2D1_COLOR_F {
                        r: 0.58,
                        g: 0.28,
                        b: 0.96,
                        a: 0.85,
                    },
                },
            ];
            let stop_col = rt
                .CreateGradientStopCollection(&border_stops, D2D1_GAMMA_2_2, D2D1_EXTEND_MODE_CLAMP)
                .map_err(|e| format!("Gagal membuat GradientStopCollection: {e}"))?;
            let lin_props = D2D1_LINEAR_GRADIENT_BRUSH_PROPERTIES {
                startPoint: D2D_POINT_2F { x: 0.0, y: 0.0 },
                endPoint: D2D_POINT_2F {
                    x: self.width_f(),
                    y: self.height_f(),
                },
            };
            let edge_grad = rt
                .CreateLinearGradientBrush(&lin_props, None, &stop_col)
                .map_err(|e| format!("Gagal membuat LinearGradientBrush: {e}"))?;

            self.brushes = Some(CachedBrushes {
                solid,
                orb_purple,
                orb_pink,
                orb_blue,
                edge_grad,
            });
            self.render_target = Some(rt);
            Ok(())
        }
    }

    fn poll_ocr_result(&mut self) {
        if let Some(rx) = &self.ocr_rx {
            if let Ok(res) = rx.try_recv() {
                self.ocr_loading = false;
                self.ocr_rx = None;
                match res {
                    Ok(layout) => {
                        self.line_capsules = compute_line_capsules(&layout);
                        self.ocr_layout = layout;
                        self.ocr_ready_instant = Some(Instant::now());
                        if self.is_dragging {
                            self.update_selection();
                        }
                    }
                    Err(err) => {
                        self.ocr_error = Some(err);
                    }
                }
            }
        }
    }

    fn poll_ai_result(&mut self) {
        if let Some(rx) = &self.ai_rx {
            if let Ok(res) = rx.try_recv() {
                self.ai_loading = false;
                self.ai_rx = None;
                match res {
                    Ok(ans) => {
                        let cfg = AiConfig::load();
                        if cfg.auto_copy_answer && !ans.copy_payload.is_empty() {
                            if copy_text_to_clipboard(&ans.copy_payload).is_ok() {
                                self.status_toast = Some((
                                    "Jawaban otomatis disalin ke Clipboard".to_string(),
                                    Instant::now(),
                                ));
                            }
                        }
                        self.ai_answer = Some(ans);
                    }
                    Err(err) => {
                        self.ai_error = Some(err);
                    }
                }
                unsafe {
                    let _ = InvalidateRect(self.hwnd, None, false);
                }
            }
        }
    }

    fn effective_mode(&self) -> SelectionMode {
        unsafe {
            let shift_held = (GetAsyncKeyState(VK_SHIFT.0 as i32) as u16 & 0x8000) != 0;
            let alt_held = (GetAsyncKeyState(VK_MENU.0 as i32) as u16 & 0x8000) != 0;
            if shift_held || alt_held {
                match self.selection_mode {
                    SelectionMode::Box => SelectionMode::Linear,
                    SelectionMode::Linear => SelectionMode::Box,
                }
            } else {
                self.selection_mode
            }
        }
    }

    fn update_selection(&mut self) {
        let mode = self.effective_mode();
        self.current_selection = self.ocr_layout.compute_selection(
            self.drag_start.0,
            self.drag_start.1,
            self.drag_current.0,
            self.drag_current.1,
            mode,
        );
    }

    fn finish_copy_and_close(&mut self, text: &str) {
        match copy_text_to_clipboard(text) {
            Ok(()) => {
                let word_count = text.split_whitespace().count();
                self.status_toast = Some((
                    format!("Tersalin ke Clipboard ({word_count} kata)"),
                    Instant::now(),
                ));
                self.closing_soon = true;
                unsafe {
                    let _ = SetTimer(self.hwnd, CLOSE_DELAY_TIMER_ID, 200, None);
                    let _ = InvalidateRect(self.hwnd, None, false);
                }
            }
            Err(err) => {
                self.status_toast = Some((err, Instant::now()));
                unsafe {
                    let _ = InvalidateRect(self.hwnd, None, false);
                }
            }
        }
    }

    /// Transitions smoothly from full-screen selection into the compact Floating Stealth Answer HUD (`WS_EX_NOACTIVATE`).
    fn start_ai_solving(&mut self, crop_rect: RectF, mut ocr_text: String) {
        let cropped_frame = self.frame.crop_region(crop_rect, 1300);

        // If OCR was still running or empty when the user released the box, run fast OCR on the cropped region
        if ocr_text.trim().is_empty() {
            if let Ok(sub_layout) = recognize_frame(&cropped_frame) {
                let sub_txt = sub_layout.full_text();
                if !sub_txt.trim().is_empty() {
                    ocr_text = sub_txt;
                }
            }
        }

        let png_b64 = cropped_frame.to_png_base64().ok();
        let config = AiConfig::load();
        self.captured_ocr_text = ocr_text.clone();
        self.ai_loading = true;
        self.ai_start_instant = Some(Instant::now());
        self.ai_answer = None;
        self.ai_error = None;
        self.hud_scroll_y = 0.0;

        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            let res = solve_question_hybrid(&config, png_b64.as_deref(), &ocr_text);
            let _ = tx.send(res);
        });
        self.ai_rx = Some(rx);

        // Compute smart non-overlapping screen placement for the Floating Stealth Answer HUD
        let mon_w = self.frame.width as i32;
        let mon_h = self.frame.height as i32;
        let hud_w = HUD_WIDTH as i32;
        let hud_h = HUD_HEIGHT as i32;

        let sel_right = crop_rect.right() as i32;
        let sel_left = crop_rect.x as i32;
        let sel_top = crop_rect.y as i32;
        let sel_bottom = crop_rect.bottom() as i32;

        // Prefer placing below the selected box; if not enough vertical room, place to the right or above
        let (local_x, local_y) = if sel_bottom + 14 + hud_h <= mon_h - 16 {
            let x = sel_left.clamp(16, (mon_w - hud_w - 16).max(16));
            (x, sel_bottom + 14)
        } else if sel_right + 14 + hud_w <= mon_w - 16 {
            let y = sel_top.clamp(16, (mon_h - hud_h - 16).max(16));
            (sel_right + 14, y)
        } else if sel_top - 14 - hud_h >= 16 {
            let x = sel_left.clamp(16, (mon_w - hud_w - 16).max(16));
            (x, sel_top - 14 - hud_h)
        } else {
            (
                (mon_w - hud_w - 24).max(16),
                (mon_h - hud_h - 48).max(16),
            )
        };

        let screen_x = self.frame.monitor_x + local_x;
        let screen_y = self.frame.monitor_y + local_y;

        // Free full-screen bitmap memory immediately since we only render the compact HUD now
        self.phase = OverlayPhase::Answering;
        self.screen_bitmap = None;
        self.frame.pixels_bgra.clear();
        self.frame.pixels_bgra.shrink_to_fit();
        set_overlay_fullscreen_capturing(false);

        unsafe {
            let round_pref = DWMWCP_ROUND;
            let _ = DwmSetWindowAttribute(
                self.hwnd,
                DWMWA_WINDOW_CORNER_PREFERENCE,
                &round_pref as *const _ as *const c_void,
                4,
            );

            let _ = SetWindowPos(
                self.hwnd,
                HWND_TOPMOST,
                screen_x,
                screen_y,
                hud_w,
                hud_h,
                SWP_NOACTIVATE,
            );

            if let Some(rt) = &self.render_target {
                let _ = rt.Resize(&D2D_SIZE_U {
                    width: HUD_WIDTH,
                    height: HUD_HEIGHT,
                });
            }
            let _ = InvalidateRect(self.hwnd, None, false);
        }
    }

    fn render(&mut self) {
        self.poll_ocr_result();
        self.poll_ai_result();

        if self.ensure_render_target().is_err() {
            return;
        }

        if self.phase == OverlayPhase::Answering {
            self.render_answering_hud();
            return;
        }

        self.glow_pos.0 += (self.hover_pos.0 - self.glow_pos.0) * 0.30;
        self.glow_pos.1 += (self.hover_pos.1 - self.glow_pos.1) * 0.30;

        let elapsed = self.start_instant.elapsed().as_secs_f32();
        let intro_t = (elapsed / 0.32).clamp(0.0, 1.0);
        let intro_ease = 1.0 - (1.0 - intro_t).powi(3);

        let w = self.width_f();
        let h = self.height_f();
        let (gx, gy) = self.glow_pos;

        let Some(rt) = self.render_target.clone() else {
            return;
        };

        unsafe {
            rt.BeginDraw();

            if let Some(bmp) = &self.screen_bitmap {
                let dest = D2D_RECT_F {
                    left: 0.0,
                    top: 0.0,
                    right: w,
                    bottom: h,
                };
                rt.DrawBitmap(
                    bmp,
                    Some(&dest),
                    1.0,
                    windows::Win32::Graphics::Direct2D::D2D1_BITMAP_INTERPOLATION_MODE_NEAREST_NEIGHBOR,
                    None,
                );
            }

            if let Some(b) = &self.brushes {
                b.solid.SetColor(&D2D1_COLOR_F {
                    r: 0.015,
                    g: 0.060,
                    b: 0.135,
                    a: 0.48 * intro_ease,
                });
                rt.FillRectangle(
                    &D2D_RECT_F {
                        left: 0.0,
                        top: 0.0,
                        right: w,
                        bottom: h,
                    },
                    &b.solid,
                );

                paint_cached_orb(&rt, &b.orb_pink, gx + 34.0, gy - 22.0, 195.0, intro_ease);
                paint_cached_orb(&rt, &b.orb_blue, gx - 38.0, gy + 20.0, 205.0, intro_ease);
                paint_cached_orb(&rt, &b.orb_purple, gx, gy, 175.0, intro_ease);

                draw_cached_edge_aura(&rt, &b.edge_grad, w, h, intro_ease);
            }

            self.draw_ocr_capsules_and_selection(&rt, gx, gy);
            self.draw_floating_ui(&rt);

            let _ = rt.EndDraw(None, None);
        }
    }

    fn render_answering_hud(&mut self) {
        let Some(rt) = self.render_target.clone() else {
            return;
        };
        let Some(b) = &self.brushes else {
            return;
        };
        let solid = &b.solid;
        let w = HUD_WIDTH as f32;
        let h = HUD_HEIGHT as f32;
        let (hx, hy) = self.hover_pos;

        unsafe {
            rt.BeginDraw();

            // Base #111111 matte carbon surface
            rt.Clear(Some(&D2D1_COLOR_F {
                r: 0.067,
                g: 0.067,
                b: 0.067,
                a: 1.0,
            }));

            // Outer border (#2D2D2D)
            solid.SetColor(&D2D1_COLOR_F {
                r: 0.22,
                g: 0.22,
                b: 0.22,
                a: 1.0,
            });
            rt.DrawRoundedRectangle(
                &D2D1_ROUNDED_RECT {
                    rect: D2D_RECT_F {
                        left: 1.0,
                        top: 1.0,
                        right: w - 1.0,
                        bottom: h - 1.0,
                    },
                    radiusX: 8.0,
                    radiusY: 8.0,
                },
                solid,
                1.5,
                None,
            );

            // Top Header Bar (#181818)
            let header = self.hud_header_rect();
            solid.SetColor(&D2D1_COLOR_F {
                r: 0.095,
                g: 0.095,
                b: 0.095,
                a: 1.0,
            });
            rt.FillRectangle(&to_d2d_rect(header), solid);

            // Header divider line
            solid.SetColor(&D2D1_COLOR_F {
                r: 0.16,
                g: 0.16,
                b: 0.16,
                a: 1.0,
            });
            rt.DrawLine(
                D2D_POINT_2F { x: 0.0, y: 42.0 },
                D2D_POINT_2F { x: w, y: 42.0 },
                solid,
                1.0,
                None,
            );

            // Status Dot & Title in Header
            let (dot_r, dot_g, dot_b, header_title) = if self.ai_loading {
                (0.22, 0.74, 0.97, "AI-Task  ·  Menganalisis Soal...".to_string())
            } else if let Some(ans) = &self.ai_answer {
                let kind = if ans.is_multiple_choice {
                    "Pilihan Ganda"
                } else {
                    "Jawaban Uraian"
                };
                (
                    0.133,
                    0.773,
                    0.369,
                    format!("AI-Task  ·  {kind} ({})", ans.model_used),
                )
            } else {
                (0.937, 0.267, 0.267, "AI-Task  ·  Perlu Konfigurasi".to_string())
            };

            solid.SetColor(&D2D1_COLOR_F {
                r: dot_r,
                g: dot_g,
                b: dot_b,
                a: 1.0,
            });
            rt.FillEllipse(
                &D2D1_ELLIPSE {
                    point: D2D_POINT_2F { x: 20.0, y: 21.0 },
                    radiusX: 4.5,
                    radiusY: 4.5,
                },
                solid,
            );

            solid.SetColor(&D2D1_COLOR_F {
                r: 0.949,
                g: 0.949,
                b: 0.949,
                a: 1.0,
            });
            draw_left_text(
                &rt,
                &self.text_format_hud_title,
                &header_title,
                RectF::new(32.0, 0.0, w - 80.0, 42.0),
                solid,
                false,
            );

            // Header Close Button `✕`
            let close_r = self.hud_close_btn_rect();
            if close_r.contains(hx, hy) {
                solid.SetColor(&D2D1_COLOR_F {
                    r: 0.22,
                    g: 0.12,
                    b: 0.12,
                    a: 1.0,
                });
                rt.FillRoundedRectangle(
                    &D2D1_ROUNDED_RECT {
                        rect: to_d2d_rect(close_r),
                        radiusX: 6.0,
                        radiusY: 6.0,
                    },
                    solid,
                );
            }
            solid.SetColor(&D2D1_COLOR_F {
                r: 0.85,
                g: 0.85,
                b: 0.85,
                a: 1.0,
            });
            draw_centered_text(&rt, &self.text_format_ui, "✕", close_r, solid);

            // ─────────────────────────────────────────────────────────────
            // HUD Main Content Area (y: 52..254)
            // ─────────────────────────────────────────────────────────────
            if self.ai_loading {
                let elapsed = self
                    .ai_start_instant
                    .map(|t| t.elapsed().as_secs_f32())
                    .unwrap_or(0.0);

                let box_r = RectF::new(16.0, 56.0, w - 32.0, 84.0);
                solid.SetColor(&D2D1_COLOR_F {
                    r: 0.106,
                    g: 0.106,
                    b: 0.106,
                    a: 1.0,
                });
                rt.FillRoundedRectangle(
                    &D2D1_ROUNDED_RECT {
                        rect: to_d2d_rect(box_r),
                        radiusX: 6.0,
                        radiusY: 6.0,
                    },
                    solid,
                );

                // Animated loading bar inside box_r
                let track_w = box_r.width - 32.0;
                let bar_w = track_w * 0.35;
                let phase_t = (elapsed * 1.4).sin() * 0.5 + 0.5;
                let bar_x = box_r.x + 16.0 + phase_t * (track_w - bar_w);

                solid.SetColor(&D2D1_COLOR_F {
                    r: 0.22,
                    g: 0.74,
                    b: 0.97,
                    a: 0.90,
                });
                rt.FillRoundedRectangle(
                    &D2D1_ROUNDED_RECT {
                        rect: D2D_RECT_F {
                            left: bar_x,
                            top: box_r.y + 18.0,
                            right: bar_x + bar_w,
                            bottom: box_r.y + 23.0,
                        },
                        radiusX: 2.5,
                        radiusY: 2.5,
                    },
                    solid,
                );

                solid.SetColor(&D2D1_COLOR_F {
                    r: 0.949,
                    g: 0.949,
                    b: 0.949,
                    a: 1.0,
                });
                draw_centered_text(
                    &rt,
                    &self.text_format_ui,
                    &format!("Mengirim gambar & teks soal ke AI ({elapsed:.1} dtk)..."),
                    RectF::new(box_r.x + 12.0, box_r.y + 32.0, box_r.width - 24.0, 24.0),
                    solid,
                );

                // Show preview of detected OCR text below
                let preview = if self.captured_ocr_text.trim().is_empty() {
                    "Mode Vision: Mengirim gambar area yang diblok langsung ke model AI."
                        .to_string()
                } else {
                    format!("Teks Soal Terdeteksi:\n{}", self.captured_ocr_text.trim())
                };
                solid.SetColor(&D2D1_COLOR_F {
                    r: 0.635,
                    g: 0.635,
                    b: 0.635,
                    a: 1.0,
                });
                draw_left_text(
                    &rt,
                    &self.text_format_hud_small_left,
                    &preview,
                    RectF::new(18.0, 152.0, w - 36.0, 96.0),
                    solid,
                    true,
                );
            } else if let Some(err) = &self.ai_error {
                let err_box = RectF::new(16.0, 54.0, w - 32.0, 194.0);
                solid.SetColor(&D2D1_COLOR_F {
                    r: 0.16,
                    g: 0.08,
                    b: 0.08,
                    a: 1.0,
                });
                rt.FillRoundedRectangle(
                    &D2D1_ROUNDED_RECT {
                        rect: to_d2d_rect(err_box),
                        radiusX: 6.0,
                        radiusY: 6.0,
                    },
                    solid,
                );
                solid.SetColor(&D2D1_COLOR_F {
                    r: 0.55,
                    g: 0.20,
                    b: 0.20,
                    a: 1.0,
                });
                rt.DrawRoundedRectangle(
                    &D2D1_ROUNDED_RECT {
                        rect: to_d2d_rect(err_box),
                        radiusX: 6.0,
                        radiusY: 6.0,
                    },
                    solid,
                    1.2,
                    None,
                );

                solid.SetColor(&D2D1_COLOR_F {
                    r: 0.95,
                    g: 0.95,
                    b: 0.95,
                    a: 1.0,
                });
                draw_left_text(
                    &rt,
                    &self.text_format_hud_body,
                    err,
                    RectF::new(err_box.x + 14.0, err_box.y + 12.0, err_box.width - 28.0, err_box.height - 24.0),
                    solid,
                    true,
                );
            } else if let Some(ans) = &self.ai_answer.clone() {
                let viewport = RectF::new(14.0, 48.0, w - 28.0, h - 98.0);
                let card_w = viewport.width - 8.0; // leave 8px on right for scrollbar track

                if ans.is_multiple_choice {
                    let text_inner_w = (card_w - 28.0).max(80.0);
                    let headline_h = measure_text_height(
                        &self.dwrite_factory,
                        &self.text_format_hud_answer_big,
                        &ans.headline,
                        text_inner_w,
                    )
                    .max(22.0);

                    let ans_box_h = 38.0 + headline_h + 14.0;
                    let detail_txt = if ans.body.is_empty() {
                        "Jawaban pilihan ganda di atas sudah siap dipilih dan otomatis disalin ke Clipboard."
                    } else {
                        ans.body.as_str()
                    };
                    let body_h = measure_text_height(
                        &self.dwrite_factory,
                        &self.text_format_hud_body,
                        detail_txt,
                        text_inner_w,
                    )
                    .max(20.0);
                    let body_box_h = (28.0 + body_h + 12.0).max(58.0);

                    let total_content_h = ans_box_h + 10.0 + body_box_h;
                    self.hud_max_scroll_y = (total_content_h - viewport.height).max(0.0);
                    self.hud_scroll_y = self.hud_scroll_y.clamp(0.0, self.hud_max_scroll_y);

                    rt.PushAxisAlignedClip(
                        &to_d2d_rect(viewport),
                        windows::Win32::Graphics::Direct2D::D2D1_ANTIALIAS_MODE_PER_PRIMITIVE,
                    );

                    let y0 = viewport.y - self.hud_scroll_y;
                    let ans_box = RectF::new(viewport.x, y0, card_w, ans_box_h);
                    solid.SetColor(&D2D1_COLOR_F {
                        r: 0.055,
                        g: 0.175,
                        b: 0.105,
                        a: 1.0,
                    });
                    rt.FillRoundedRectangle(
                        &D2D1_ROUNDED_RECT {
                            rect: to_d2d_rect(ans_box),
                            radiusX: 6.0,
                            radiusY: 6.0,
                        },
                        solid,
                    );
                    solid.SetColor(&D2D1_COLOR_F {
                        r: 0.133,
                        g: 0.773,
                        b: 0.369,
                        a: 1.0,
                    });
                    rt.DrawRoundedRectangle(
                        &D2D1_ROUNDED_RECT {
                            rect: to_d2d_rect(ans_box),
                            radiusX: 6.0,
                            radiusY: 6.0,
                        },
                        solid,
                        1.5,
                        None,
                    );

                    // Prominent Option Badge at top-left (e.g. "OPSI B") + Label
                    let mut label_x = ans_box.x + 14.0;
                    if let Some(opt_letter) = extract_option_letter(&ans.headline) {
                        let badge_rect = RectF::new(ans_box.x + 14.0, ans_box.y + 8.0, 68.0, 22.0);
                        solid.SetColor(&D2D1_COLOR_F {
                            r: 0.133,
                            g: 0.773,
                            b: 0.369,
                            a: 1.0,
                        });
                        rt.FillRoundedRectangle(
                            &D2D1_ROUNDED_RECT {
                                rect: to_d2d_rect(badge_rect),
                                radiusX: 4.5,
                                radiusY: 4.5,
                            },
                            solid,
                        );
                        solid.SetColor(&D2D1_COLOR_F {
                            r: 0.05,
                            g: 0.08,
                            b: 0.06,
                            a: 1.0,
                        });
                        draw_centered_text(
                            &rt,
                            &self.text_format_ui,
                            &format!("OPSI {opt_letter}"),
                            badge_rect,
                            solid,
                        );
                        label_x = badge_rect.right() + 10.0;
                    }

                    solid.SetColor(&D2D1_COLOR_F {
                        r: 0.35,
                        g: 0.92,
                        b: 0.56,
                        a: 1.0,
                    });
                    draw_left_text(
                        &rt,
                        &self.text_format_hud_small_left,
                        "JAWABAN PILIHAN GANDA",
                        RectF::new(
                            label_x,
                            ans_box.y + 11.0,
                            (ans_box.right() - label_x - 12.0).max(40.0),
                            18.0,
                        ),
                        solid,
                        false,
                    );

                    // Top-aligned full option text (never clipped at top or bottom!)
                    solid.SetColor(&D2D1_COLOR_F {
                        r: 0.97,
                        g: 0.99,
                        b: 0.98,
                        a: 1.0,
                    });
                    draw_left_text(
                        &rt,
                        &self.text_format_hud_answer_big,
                        &ans.headline,
                        RectF::new(
                            ans_box.x + 14.0,
                            ans_box.y + 36.0,
                            text_inner_w,
                            headline_h + 8.0,
                        ),
                        solid,
                        false,
                    );

                    // Explanation Box directly below ans_box (scrolls together with ans_box!)
                    let body_rect = RectF::new(
                        viewport.x,
                        ans_box.bottom() + 10.0,
                        card_w,
                        body_box_h,
                    );
                    solid.SetColor(&D2D1_COLOR_F {
                        r: 0.106,
                        g: 0.106,
                        b: 0.106,
                        a: 1.0,
                    });
                    rt.FillRoundedRectangle(
                        &D2D1_ROUNDED_RECT {
                            rect: to_d2d_rect(body_rect),
                            radiusX: 6.0,
                            radiusY: 6.0,
                        },
                        solid,
                    );
                    solid.SetColor(&D2D1_COLOR_F {
                        r: 0.18,
                        g: 0.18,
                        b: 0.18,
                        a: 1.0,
                    });
                    rt.DrawRoundedRectangle(
                        &D2D1_ROUNDED_RECT {
                            rect: to_d2d_rect(body_rect),
                            radiusX: 6.0,
                            radiusY: 6.0,
                        },
                        solid,
                        1.0,
                        None,
                    );

                    solid.SetColor(&D2D1_COLOR_F {
                        r: 0.635,
                        g: 0.635,
                        b: 0.635,
                        a: 1.0,
                    });
                    draw_left_text(
                        &rt,
                        &self.text_format_hud_small_left,
                        "ALASAN / PEMBAHASAN SINGKAT:",
                        RectF::new(body_rect.x + 14.0, body_rect.y + 8.0, text_inner_w, 16.0),
                        solid,
                        false,
                    );

                    solid.SetColor(&D2D1_COLOR_F {
                        r: 0.88,
                        g: 0.88,
                        b: 0.88,
                        a: 1.0,
                    });
                    draw_left_text(
                        &rt,
                        &self.text_format_hud_body,
                        detail_txt,
                        RectF::new(
                            body_rect.x + 14.0,
                            body_rect.y + 27.0,
                            text_inner_w,
                            body_h + 8.0,
                        ),
                        solid,
                        false,
                    );

                    rt.PopAxisAlignedClip();
                } else {
                    // Essay / Explanation Question Format (Unified Clipped Scrollable View)
                    let text_inner_w = (card_w - 28.0).max(80.0);
                    let headline_h = measure_text_height(
                        &self.dwrite_factory,
                        &self.text_format_hud_answer_big,
                        &ans.headline,
                        text_inner_w,
                    )
                    .max(20.0);
                    let head_box_h = 34.0 + headline_h + 12.0;

                    let body_h = measure_text_height(
                        &self.dwrite_factory,
                        &self.text_format_hud_body,
                        &ans.body,
                        text_inner_w,
                    )
                    .max(40.0);
                    let exp_box_h = (32.0 + body_h + 14.0).max(110.0);

                    let total_content_h = head_box_h + 10.0 + exp_box_h;
                    self.hud_max_scroll_y = (total_content_h - viewport.height).max(0.0);
                    self.hud_scroll_y = self.hud_scroll_y.clamp(0.0, self.hud_max_scroll_y);

                    rt.PushAxisAlignedClip(
                        &to_d2d_rect(viewport),
                        windows::Win32::Graphics::Direct2D::D2D1_ANTIALIAS_MODE_PER_PRIMITIVE,
                    );

                    let y0 = viewport.y - self.hud_scroll_y;
                    let head_box = RectF::new(viewport.x, y0, card_w, head_box_h);
                    solid.SetColor(&D2D1_COLOR_F {
                        r: 0.08,
                        g: 0.14,
                        b: 0.20,
                        a: 1.0,
                    });
                    rt.FillRoundedRectangle(
                        &D2D1_ROUNDED_RECT {
                            rect: to_d2d_rect(head_box),
                            radiusX: 6.0,
                            radiusY: 6.0,
                        },
                        solid,
                    );
                    solid.SetColor(&D2D1_COLOR_F {
                        r: 0.22,
                        g: 0.74,
                        b: 0.97,
                        a: 0.9,
                    });
                    rt.DrawRoundedRectangle(
                        &D2D1_ROUNDED_RECT {
                            rect: to_d2d_rect(head_box),
                            radiusX: 6.0,
                            radiusY: 6.0,
                        },
                        solid,
                        1.2,
                        None,
                    );

                    draw_left_text(
                        &rt,
                        &self.text_format_hud_small_left,
                        "INTI JAWABAN:",
                        RectF::new(head_box.x + 14.0, head_box.y + 8.0, text_inner_w, 16.0),
                        solid,
                        false,
                    );

                    solid.SetColor(&D2D1_COLOR_F {
                        r: 0.96,
                        g: 0.98,
                        b: 1.0,
                        a: 1.0,
                    });
                    draw_left_text(
                        &rt,
                        &self.text_format_hud_answer_big,
                        &ans.headline,
                        RectF::new(
                            head_box.x + 14.0,
                            head_box.y + 28.0,
                            text_inner_w,
                            headline_h + 8.0,
                        ),
                        solid,
                        false,
                    );

                    let exp_box = RectF::new(
                        viewport.x,
                        head_box.bottom() + 10.0,
                        card_w,
                        exp_box_h,
                    );
                    solid.SetColor(&D2D1_COLOR_F {
                        r: 0.106,
                        g: 0.106,
                        b: 0.106,
                        a: 1.0,
                    });
                    rt.FillRoundedRectangle(
                        &D2D1_ROUNDED_RECT {
                            rect: to_d2d_rect(exp_box),
                            radiusX: 6.0,
                            radiusY: 6.0,
                        },
                        solid,
                    );
                    solid.SetColor(&D2D1_COLOR_F {
                        r: 0.18,
                        g: 0.18,
                        b: 0.18,
                        a: 1.0,
                    });
                    rt.DrawRoundedRectangle(
                        &D2D1_ROUNDED_RECT {
                            rect: to_d2d_rect(exp_box),
                            radiusX: 6.0,
                            radiusY: 6.0,
                        },
                        solid,
                        1.0,
                        None,
                    );

                    solid.SetColor(&D2D1_COLOR_F {
                        r: 0.635,
                        g: 0.635,
                        b: 0.635,
                        a: 1.0,
                    });
                    draw_left_text(
                        &rt,
                        &self.text_format_hud_small_left,
                        "PENJELASAN LENGKAP:",
                        RectF::new(exp_box.x + 14.0, exp_box.y + 8.0, text_inner_w, 16.0),
                        solid,
                        false,
                    );

                    solid.SetColor(&D2D1_COLOR_F {
                        r: 0.93,
                        g: 0.93,
                        b: 0.93,
                        a: 1.0,
                    });
                    draw_left_text(
                        &rt,
                        &self.text_format_hud_body,
                        &ans.body,
                        RectF::new(
                            exp_box.x + 14.0,
                            exp_box.y + 28.0,
                            text_inner_w,
                            body_h + 8.0,
                        ),
                        solid,
                        false,
                    );

                    rt.PopAxisAlignedClip();
                }

                // Draw vertical scrollbar indicator on right edge if content overflows viewport
                if self.hud_max_scroll_y > 1.0 {
                    let track_x = viewport.right() - 4.0;
                    let track_h = viewport.height;
                    let total_h = viewport.height + self.hud_max_scroll_y;
                    let thumb_h = ((viewport.height / total_h) * track_h).clamp(24.0, track_h);
                    let scroll_ratio = (self.hud_scroll_y / self.hud_max_scroll_y).clamp(0.0, 1.0);
                    let thumb_y = viewport.y + scroll_ratio * (track_h - thumb_h);

                    solid.SetColor(&D2D1_COLOR_F {
                        r: 0.14,
                        g: 0.14,
                        b: 0.14,
                        a: 1.0,
                    });
                    rt.FillRoundedRectangle(
                        &D2D1_ROUNDED_RECT {
                            rect: D2D_RECT_F {
                                left: track_x,
                                top: viewport.y,
                                right: track_x + 4.0,
                                bottom: viewport.bottom(),
                            },
                            radiusX: 2.0,
                            radiusY: 2.0,
                        },
                        solid,
                    );

                    solid.SetColor(&D2D1_COLOR_F {
                        r: 0.45,
                        g: 0.45,
                        b: 0.45,
                        a: 1.0,
                    });
                    rt.FillRoundedRectangle(
                        &D2D1_ROUNDED_RECT {
                            rect: D2D_RECT_F {
                                left: track_x,
                                top: thumb_y,
                                right: track_x + 4.0,
                                bottom: thumb_y + thumb_h,
                            },
                            radiusX: 2.0,
                            radiusY: 2.0,
                        },
                        solid,
                    );
                }
            }

            // ─────────────────────────────────────────────────────────────
            // HUD Bottom Action Bar
            // ─────────────────────────────────────────────────────────────
            let copy_ans_r = self.hud_btn_copy_answer_rect();
            let copy_ocr_r = self.hud_btn_copy_ocr_rect();
            let up_r = self.hud_btn_scroll_up_rect();
            let dn_r = self.hud_btn_scroll_down_rect();
            let dismiss_r = self.hud_btn_dismiss_rect();

            // Primary CTA: Salin Jawaban (#F2F2F2 solid)
            if copy_ans_r.contains(hx, hy) {
                solid.SetColor(&D2D1_COLOR_F {
                    r: 0.85,
                    g: 0.85,
                    b: 0.85,
                    a: 1.0,
                });
            } else {
                solid.SetColor(&D2D1_COLOR_F {
                    r: 0.949,
                    g: 0.949,
                    b: 0.949,
                    a: 1.0,
                });
            }
            rt.FillRoundedRectangle(
                &D2D1_ROUNDED_RECT {
                    rect: to_d2d_rect(copy_ans_r),
                    radiusX: 6.0,
                    radiusY: 6.0,
                },
                solid,
            );
            solid.SetColor(&D2D1_COLOR_F {
                r: 0.067,
                g: 0.067,
                b: 0.067,
                a: 1.0,
            });
            let copy_label = if let Some((toast, t)) = &self.status_toast {
                if t.elapsed().as_secs_f32() < 2.2 && toast.contains("Jawaban") {
                    "Tersalin!"
                } else {
                    "Salin Jawaban"
                }
            } else {
                "Salin Jawaban"
            };
            draw_centered_text(&rt, &self.text_format_ui, copy_label, copy_ans_r, solid);

            // Secondary buttons
            for (rect, label) in [
                (copy_ocr_r, "Salin Teks Soal"),
                (up_r, "Atas"),
                (dn_r, "Bwh"),
                (dismiss_r, "Tutup"),
            ] {
                if rect.contains(hx, hy) {
                    solid.SetColor(&D2D1_COLOR_F {
                        r: 0.16,
                        g: 0.16,
                        b: 0.16,
                        a: 1.0,
                    });
                } else {
                    solid.SetColor(&D2D1_COLOR_F {
                        r: 0.11,
                        g: 0.11,
                        b: 0.11,
                        a: 1.0,
                    });
                }
                rt.FillRoundedRectangle(
                    &D2D1_ROUNDED_RECT {
                        rect: to_d2d_rect(rect),
                        radiusX: 6.0,
                        radiusY: 6.0,
                    },
                    solid,
                );
                solid.SetColor(&D2D1_COLOR_F {
                    r: 0.22,
                    g: 0.22,
                    b: 0.22,
                    a: 1.0,
                });
                rt.DrawRoundedRectangle(
                    &D2D1_ROUNDED_RECT {
                        rect: to_d2d_rect(rect),
                        radiusX: 6.0,
                        radiusY: 6.0,
                    },
                    solid,
                    1.0,
                    None,
                );
                solid.SetColor(&D2D1_COLOR_F {
                    r: 0.90,
                    g: 0.90,
                    b: 0.90,
                    a: 1.0,
                });
                draw_centered_text(&rt, &self.text_format_small, label, rect, solid);
            }

            let _ = rt.EndDraw(None, None);
        }
    }

    unsafe fn draw_ocr_capsules_and_selection(
        &self,
        rt: &ID2D1HwndRenderTarget,
        gx: f32,
        gy: f32,
    ) {
        let Some(b) = &self.brushes else { return };
        let solid = &b.solid;

        let ocr_elapsed = self
            .ocr_ready_instant
            .map(|t| t.elapsed().as_secs_f32())
            .unwrap_or(0.0);

        let w = self.width_f();
        let h = self.height_f();
        let max_diag = (w * w + h * h).sqrt().max(1.0);
        let click_x = self.frame.local_click_x;
        let click_y = self.frame.local_click_y;
        let reveal_done = ocr_elapsed >= 0.50;
        let reveal_radius = if reveal_done {
            max_diag * 2.0
        } else {
            ocr_elapsed * (max_diag / 0.45)
        };

        let pad_x = 6.5;
        let pad_y = 3.5;

        for cap in &self.line_capsules {
            let cx = cap.center_x();
            let cy = cap.center_y();

            let mut shimmer_alpha = 0.0_f32;
            if !reveal_done {
                let dist_click = ((cx - click_x).powi(2) + (cy - click_y).powi(2)).sqrt();
                if dist_click > reveal_radius {
                    continue;
                }
                let wave_age = (ocr_elapsed - (dist_click / (max_diag / 0.45))).max(0.0);
                if wave_age < 0.35 {
                    shimmer_alpha = (1.0 - (wave_age / 0.35)) * 0.42;
                }
            }

            let dx = if gx < cap.x {
                cap.x - gx
            } else if gx > cap.right() {
                gx - cap.right()
            } else {
                0.0
            };
            let dy = if gy < cap.y {
                cap.y - gy
            } else if gy > cap.bottom() {
                gy - cap.bottom()
            } else {
                0.0
            };

            let mut glow_alpha = shimmer_alpha;
            if dx < 190.0 && dy < 190.0 {
                let dist_sq = dx * dx + dy * dy;
                if dist_sq < 190.0 * 190.0 {
                    let prox = 1.0 - (dist_sq.sqrt() / 190.0);
                    glow_alpha = (glow_alpha + prox * prox * 0.48).min(0.65);
                }
            }

            let pill_rect = D2D_RECT_F {
                left: (cap.x - pad_x).max(0.0),
                top: (cap.y - pad_y).max(0.0),
                right: (cap.right() + pad_x).min(w),
                bottom: (cap.bottom() + pad_y).min(h),
            };

            if glow_alpha > 0.03 {
                solid.SetColor(&D2D1_COLOR_F {
                    r: 0.22,
                    g: 0.74,
                    b: 0.98,
                    a: glow_alpha * 0.55,
                });
                rt.DrawRoundedRectangle(
                    &D2D1_ROUNDED_RECT {
                        rect: D2D_RECT_F {
                            left: pill_rect.left - 3.0,
                            top: pill_rect.top - 3.0,
                            right: pill_rect.right + 3.0,
                            bottom: pill_rect.bottom + 3.0,
                        },
                        radiusX: 9.0,
                        radiusY: 9.0,
                    },
                    solid,
                    4.5,
                    None,
                );
            }

            if let Some(bmp) = &self.screen_bitmap {
                rt.DrawBitmap(
                    bmp,
                    Some(&pill_rect),
                    1.0,
                    windows::Win32::Graphics::Direct2D::D2D1_BITMAP_INTERPOLATION_MODE_NEAREST_NEIGHBOR,
                    Some(&pill_rect),
                );
            }

            let rrect = D2D1_ROUNDED_RECT {
                rect: pill_rect,
                radiusX: 6.5,
                radiusY: 6.5,
            };
            solid.SetColor(&D2D1_COLOR_F {
                r: 0.52,
                g: 0.78,
                b: 0.98,
                a: 0.28 + glow_alpha * 0.50,
            });
            rt.DrawRoundedRectangle(&rrect, solid, 1.1, None);
        }

        if !self.is_dragging {
            if let Some(hovered) = self
                .ocr_layout
                .word_at_point(self.hover_pos.0, self.hover_pos.1)
            {
                let r = hovered.bounds;
                solid.SetColor(&D2D1_COLOR_F {
                    r: 0.56,
                    g: 0.80,
                    b: 1.0,
                    a: 0.70,
                });
                rt.DrawRoundedRectangle(
                    &D2D1_ROUNDED_RECT {
                        rect: D2D_RECT_F {
                            left: r.x - 2.5,
                            top: r.y - 2.0,
                            right: r.right() + 2.5,
                            bottom: r.bottom() + 2.0,
                        },
                        radiusX: 4.5,
                        radiusY: 4.5,
                    },
                    solid,
                    1.3,
                    None,
                );
            }
        }

        if !self.current_selection.highlight_rects.is_empty() {
            for rect in &self.current_selection.highlight_rects {
                let pad = 3.0;
                let rrect = D2D1_ROUNDED_RECT {
                    rect: D2D_RECT_F {
                        left: rect.x - pad,
                        top: rect.y - pad,
                        right: rect.right() + pad,
                        bottom: rect.bottom() + pad,
                    },
                    radiusX: 5.5,
                    radiusY: 5.5,
                };
                solid.SetColor(&D2D1_COLOR_F {
                    r: 0.133,
                    g: 0.773,
                    b: 0.369,
                    a: 0.18,
                });
                rt.FillRoundedRectangle(&rrect, solid);

                solid.SetColor(&D2D1_COLOR_F {
                    r: 0.22,
                    g: 0.88,
                    b: 0.52,
                    a: 0.98,
                });
                rt.DrawRoundedRectangle(&rrect, solid, 2.0, None);
            }
        }

        // In Box mode, restore 100% crisp un-tinted pixels inside the dragged box so the user sees the question clearly!
        if self.is_dragging && self.effective_mode() == SelectionMode::Box {
            let box_rect = RectF::from_points(
                self.drag_start.0,
                self.drag_start.1,
                self.drag_current.0,
                self.drag_current.1,
            );
            let d2d_box = D2D_RECT_F {
                left: box_rect.x,
                top: box_rect.y,
                right: box_rect.right(),
                bottom: box_rect.bottom(),
            };
            if box_rect.width > 4.0 && box_rect.height > 4.0 {
                if let Some(bmp) = &self.screen_bitmap {
                    rt.DrawBitmap(
                        bmp,
                        Some(&d2d_box),
                        1.0,
                        windows::Win32::Graphics::Direct2D::D2D1_BITMAP_INTERPOLATION_MODE_NEAREST_NEIGHBOR,
                        Some(&d2d_box),
                    );
                }
            }
            solid.SetColor(&D2D1_COLOR_F {
                r: 0.22,
                g: 0.74,
                b: 0.97,
                a: 0.10,
            });
            rt.FillRectangle(&d2d_box, solid);

            solid.SetColor(&D2D1_COLOR_F {
                r: 0.22,
                g: 0.74,
                b: 0.97,
                a: 0.96,
            });
            rt.DrawRectangle(&d2d_box, solid, 2.0, None);
        }
    }

    unsafe fn draw_floating_ui(&self, rt: &ID2D1HwndRenderTarget) {
        let Some(b) = &self.brushes else { return };
        let solid = &b.solid;

        let bar = self.top_toolbar_rect();
        let pill = self.status_pill_rect();

        let bar_rrect = D2D1_ROUNDED_RECT {
            rect: to_d2d_rect(bar),
            radiusX: 10.0,
            radiusY: 10.0,
        };
        solid.SetColor(&D2D1_COLOR_F {
            r: 0.067,
            g: 0.067,
            b: 0.067,
            a: 0.94,
        });
        rt.FillRoundedRectangle(&bar_rrect, solid);

        solid.SetColor(&D2D1_COLOR_F {
            r: 0.20,
            g: 0.20,
            b: 0.20,
            a: 0.90,
        });
        rt.DrawRoundedRectangle(&bar_rrect, solid, 1.2, None);

        let effective = self.effective_mode();
        let (hx, hy) = self.hover_pos;
        let btn_radius = 7.0;

        for (rect, label, is_active) in [
            (
                self.btn_box_rect(),
                "Kotak Area",
                effective == SelectionMode::Box,
            ),
            (
                self.btn_linear_rect(),
                "Teks Linear",
                effective == SelectionMode::Linear,
            ),
            (self.btn_solve_all_rect(), "Jawab Layar", false),
            (self.btn_copy_all_rect(), "Salin Teks", false),
            (self.btn_close_rect(), "✕", false),
        ] {
            if is_active {
                solid.SetColor(&D2D1_COLOR_F {
                    r: 0.22,
                    g: 0.22,
                    b: 0.22,
                    a: 1.0,
                });
                rt.FillRoundedRectangle(
                    &D2D1_ROUNDED_RECT {
                        rect: to_d2d_rect(rect),
                        radiusX: btn_radius,
                        radiusY: btn_radius,
                    },
                    solid,
                );
            } else if rect.contains(hx, hy) {
                solid.SetColor(&D2D1_COLOR_F {
                    r: 0.14,
                    g: 0.14,
                    b: 0.14,
                    a: 0.95,
                });
                rt.FillRoundedRectangle(
                    &D2D1_ROUNDED_RECT {
                        rect: to_d2d_rect(rect),
                        radiusX: btn_radius,
                        radiusY: btn_radius,
                    },
                    solid,
                );
            }

            solid.SetColor(&D2D1_COLOR_F {
                r: 0.96,
                g: 0.98,
                b: 1.0,
                a: 1.0,
            });
            draw_centered_text(rt, &self.text_format_ui, label, rect, solid);
        }

        let status_msg = if let Some((toast, _)) = &self.status_toast {
            toast.clone()
        } else if let Some(err) = &self.ocr_error {
            err.clone()
        } else if self.ocr_loading {
            "Mendeteksi soal di layar (kamu bisa langsung blok area sekarang)...".to_string()
        } else if self.current_selection.has_valid_target() {
            let count = self.current_selection.selected_word_indices.len();
            if count > 0 {
                format!("{count} kata soal terpilih · Lepas klik untuk dijawab AI (Ctrl+C hanya salin teks)")
            } else {
                "Area gambar soal terpilih · Lepas klik untuk dikirim ke AI Vision".to_string()
            }
        } else {
            format!(
                "Blok area soal untuk dijawab AI ({} kata terdeteksi) · Esc atau Klik Kanan batal",
                self.ocr_layout.words.len()
            )
        };

        solid.SetColor(&D2D1_COLOR_F {
            r: 0.0,
            g: 0.0,
            b: 0.0,
            a: 0.85,
        });
        let shadow_rect = RectF::new(pill.x + 1.0, pill.y + 1.5, pill.width, pill.height);
        draw_centered_text(rt, &self.text_format_small, &status_msg, shadow_rect, solid);

        solid.SetColor(&D2D1_COLOR_F {
            r: 0.82,
            g: 0.88,
            b: 0.96,
            a: 0.96,
        });
        draw_centered_text(rt, &self.text_format_small, &status_msg, pill, solid);
    }
}

unsafe fn create_cached_radial_brush(
    rt: &ID2D1HwndRenderTarget,
    inner_rgba: (f32, f32, f32, f32),
    mid_rgba: (f32, f32, f32, f32),
) -> Result<ID2D1RadialGradientBrush, String> {
    let stops = [
        D2D1_GRADIENT_STOP {
            position: 0.0,
            color: D2D1_COLOR_F {
                r: inner_rgba.0,
                g: inner_rgba.1,
                b: inner_rgba.2,
                a: inner_rgba.3,
            },
        },
        D2D1_GRADIENT_STOP {
            position: 0.55,
            color: D2D1_COLOR_F {
                r: mid_rgba.0,
                g: mid_rgba.1,
                b: mid_rgba.2,
                a: mid_rgba.3,
            },
        },
        D2D1_GRADIENT_STOP {
            position: 1.0,
            color: D2D1_COLOR_F {
                r: mid_rgba.0,
                g: mid_rgba.1,
                b: mid_rgba.2,
                a: 0.0,
            },
        },
    ];
    let col = rt
        .CreateGradientStopCollection(&stops, D2D1_GAMMA_2_2, D2D1_EXTEND_MODE_CLAMP)
        .map_err(|e| format!("Gagal membuat radial stop collection: {e}"))?;
    let props = D2D1_RADIAL_GRADIENT_BRUSH_PROPERTIES {
        center: D2D_POINT_2F { x: 0.0, y: 0.0 },
        gradientOriginOffset: D2D_POINT_2F { x: 0.0, y: 0.0 },
        radiusX: 180.0,
        radiusY: 180.0,
    };
    rt.CreateRadialGradientBrush(&props, None, &col)
        .map_err(|e| format!("Gagal membuat RadialGradientBrush: {e}"))
}

unsafe fn paint_cached_orb(
    rt: &ID2D1HwndRenderTarget,
    brush: &ID2D1RadialGradientBrush,
    cx: f32,
    cy: f32,
    radius: f32,
    opacity: f32,
) {
    let pt = D2D_POINT_2F { x: cx, y: cy };
    brush.SetCenter(pt);
    brush.SetRadiusX(radius);
    brush.SetRadiusY(radius);
    brush.SetOpacity(opacity);
    rt.FillEllipse(
        &D2D1_ELLIPSE {
            point: pt,
            radiusX: radius,
            radiusY: radius,
        },
        brush,
    );
}

unsafe fn draw_cached_edge_aura(
    rt: &ID2D1HwndRenderTarget,
    grad_brush: &ID2D1LinearGradientBrush,
    w: f32,
    h: f32,
    intro_ease: f32,
) {
    let edge_thickness = 22.0;
    let aura_alpha = 0.28 * intro_ease;

    for i in 0..4 {
        let inset = i as f32 * (edge_thickness / 4.0);
        let layer_alpha = (1.0 - (i as f32 / 4.0)).powi(2) * aura_alpha;
        grad_brush.SetOpacity(layer_alpha);
        rt.DrawRectangle(
            &D2D_RECT_F {
                left: inset,
                top: inset,
                right: w - inset,
                bottom: h - inset,
            },
            grad_brush,
            edge_thickness / 3.5,
            None,
        );
    }

    grad_brush.SetOpacity(0.90 * intro_ease);
    rt.DrawRectangle(
        &D2D_RECT_F {
            left: 1.5,
            top: 1.5,
            right: w - 1.5,
            bottom: h - 1.5,
        },
        grad_brush,
        2.5,
        None,
    );
}

fn to_d2d_rect(r: RectF) -> D2D_RECT_F {
    D2D_RECT_F {
        left: r.x,
        top: r.y,
        right: r.right(),
        bottom: r.bottom(),
    }
}

unsafe fn draw_centered_text(
    rt: &ID2D1HwndRenderTarget,
    format: &IDWriteTextFormat,
    text: &str,
    rect: RectF,
    brush: &ID2D1SolidColorBrush,
) {
    let utf16: Vec<u16> = text.encode_utf16().collect();
    let d2d_rect = to_d2d_rect(rect);
    rt.DrawText(
        &utf16,
        format,
        &d2d_rect,
        brush,
        D2D1_DRAW_TEXT_OPTIONS_CLIP,
        DWRITE_MEASURING_MODE_NATURAL,
    );
}

unsafe fn draw_left_text(
    rt: &ID2D1HwndRenderTarget,
    format: &IDWriteTextFormat,
    text: &str,
    rect: RectF,
    brush: &ID2D1SolidColorBrush,
    clip: bool,
) {
    let utf16: Vec<u16> = text.encode_utf16().collect();
    let d2d_rect = to_d2d_rect(rect);
    let opts = if clip {
        D2D1_DRAW_TEXT_OPTIONS_CLIP
    } else {
        D2D1_DRAW_TEXT_OPTIONS_NONE
    };
    rt.DrawText(
        &utf16,
        format,
        &d2d_rect,
        brush,
        opts,
        DWRITE_MEASURING_MODE_NATURAL,
    );
}

/// Measures the exact pixel height of a wrapped multi-line string using DirectWrite's `IDWriteTextLayout`.
unsafe fn measure_text_height(
    dwrite_factory: &IDWriteFactory,
    format: &IDWriteTextFormat,
    text: &str,
    max_width: f32,
) -> f32 {
    if text.is_empty() {
        return 18.0;
    }
    let utf16: Vec<u16> = text.encode_utf16().collect();
    if let Ok(layout) = dwrite_factory.CreateTextLayout(&utf16, format, max_width.max(40.0), 4000.0)
    {
        let mut metrics = DWRITE_TEXT_METRICS::default();
        if layout.GetMetrics(&mut metrics).is_ok() {
            return metrics.height.ceil() + 4.0;
        }
    }
    // Fallback estimate if layout creation fails
    let approx_chars_per_line = ((max_width / 7.2).floor() as usize).max(15);
    let mut lines = 0usize;
    for l in text.lines() {
        let count = l.chars().count();
        lines += (count / approx_chars_per_line) + 1;
    }
    (lines.max(1) as f32) * 20.0
}

/// Extracts the multiple-choice option letter (`A`..`E`) from the start of an answer headline
/// (e.g. `"B. Tingginya angka..."` -> `Some('B')`).
pub fn extract_option_letter(headline: &str) -> Option<char> {
    let trimmed = headline
        .trim()
        .trim_start_matches('(')
        .trim_start_matches('[');

    let upper = trimmed.to_ascii_uppercase();
    for prefix in ["OPSI ", "PILIHAN ", "JAWABAN "] {
        if let Some(rest) = upper.strip_prefix(prefix) {
            let first = rest.trim().chars().next()?;
            if matches!(first, 'A'..='E') {
                return Some(first);
            }
        }
    }

    let mut chars = trimmed.chars();
    let first = chars.next()?.to_ascii_uppercase();
    if matches!(first, 'A'..='E') {
        match chars.next() {
            None => return Some(first),
            Some(second) if matches!(second, '.' | ')' | ']' | ':' | '-' | ' ') => {
                return Some(first);
            }
            _ => {}
        }
    }
    None
}

pub fn run_overlay_session(
    frame: CapturedFrame,
    default_mode: SelectionMode,
) -> Result<SelectionMode, String> {
    unsafe {
        let d2d_factory: ID2D1Factory =
            D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)
                .map_err(|e| format!("Gagal menginisialisasi Direct2D Factory: {e}"))?;

        let dwrite_factory: IDWriteFactory = DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)
            .map_err(|e| format!("Gagal menginisialisasi DirectWrite Factory: {e}"))?;

        let text_format_ui = dwrite_factory
            .CreateTextFormat(
                w!("Segoe UI"),
                None,
                DWRITE_FONT_WEIGHT_SEMI_BOLD,
                DWRITE_FONT_STYLE_NORMAL,
                DWRITE_FONT_STRETCH_NORMAL,
                13.0,
                w!("en-us"),
            )
            .map_err(|e| format!("Gagal membuat format font UI: {e}"))?;
        let _ = text_format_ui.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_CENTER);
        let _ = text_format_ui.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER);

        let text_format_small = dwrite_factory
            .CreateTextFormat(
                w!("Segoe UI"),
                None,
                DWRITE_FONT_WEIGHT_NORMAL,
                DWRITE_FONT_STYLE_NORMAL,
                DWRITE_FONT_STRETCH_NORMAL,
                12.0,
                w!("en-us"),
            )
            .map_err(|e| format!("Gagal membuat format font status: {e}"))?;
        let _ = text_format_small.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_CENTER);
        let _ = text_format_small.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER);

        let text_format_hud_title = dwrite_factory
            .CreateTextFormat(
                w!("Segoe UI"),
                None,
                DWRITE_FONT_WEIGHT_SEMI_BOLD,
                DWRITE_FONT_STYLE_NORMAL,
                DWRITE_FONT_STRETCH_NORMAL,
                13.0,
                w!("en-us"),
            )
            .map_err(|e| format!("Gagal membuat font HUD title: {e}"))?;
        let _ = text_format_hud_title.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_LEADING);
        let _ = text_format_hud_title.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER);

        let text_format_hud_answer_big = dwrite_factory
            .CreateTextFormat(
                w!("Segoe UI"),
                None,
                DWRITE_FONT_WEIGHT_BOLD,
                DWRITE_FONT_STYLE_NORMAL,
                DWRITE_FONT_STRETCH_NORMAL,
                14.5,
                w!("en-us"),
            )
            .map_err(|e| format!("Gagal membuat font HUD answer big: {e}"))?;
        let _ = text_format_hud_answer_big.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_LEADING);
        let _ = text_format_hud_answer_big.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_NEAR);
        let _ = text_format_hud_answer_big.SetWordWrapping(DWRITE_WORD_WRAPPING_WRAP);

        let text_format_hud_body = dwrite_factory
            .CreateTextFormat(
                w!("Segoe UI"),
                None,
                DWRITE_FONT_WEIGHT_NORMAL,
                DWRITE_FONT_STYLE_NORMAL,
                DWRITE_FONT_STRETCH_NORMAL,
                13.0,
                w!("en-us"),
            )
            .map_err(|e| format!("Gagal membuat font HUD body: {e}"))?;
        let _ = text_format_hud_body.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_LEADING);
        let _ = text_format_hud_body.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_NEAR);
        let _ = text_format_hud_body.SetWordWrapping(DWRITE_WORD_WRAPPING_WRAP);

        let text_format_hud_small_left = dwrite_factory
            .CreateTextFormat(
                w!("Segoe UI"),
                None,
                DWRITE_FONT_WEIGHT_SEMI_BOLD,
                DWRITE_FONT_STYLE_NORMAL,
                DWRITE_FONT_STRETCH_NORMAL,
                11.5,
                w!("en-us"),
            )
            .map_err(|e| format!("Gagal membuat font HUD small: {e}"))?;
        let _ = text_format_hud_small_left.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_LEADING);
        let _ = text_format_hud_small_left.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_NEAR);
        let _ = text_format_hud_small_left.SetWordWrapping(DWRITE_WORD_WRAPPING_WRAP);

        let (tx, rx) = mpsc::channel();
        let frame_for_ocr = frame.clone();
        thread::spawn(move || {
            let res = recognize_frame(&frame_for_ocr);
            let _ = tx.send(res);
        });

        let hinstance = GetModuleHandleW(None)
            .map_err(|e| format!("Gagal membaca module handle: {e}"))?;
        let class_name: PCWSTR = w!("AITaskOverlayWindowClass");

        let wc = WNDCLASSW {
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(overlay_wnd_proc),
            hInstance: hinstance.into(),
            lpszClassName: class_name,
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            ..Default::default()
        };
        let _ = RegisterClassW(&wc);

        let mon_x = frame.monitor_x;
        let mon_y = frame.monitor_y;
        let mon_w = frame.width as i32;
        let mon_h = frame.height as i32;
        let init_x = frame.local_click_x;
        let init_y = frame.local_click_y;

        let state = Box::new(OverlayState {
            hwnd: HWND::default(),
            frame,
            d2d_factory,
            dwrite_factory,
            text_format_ui,
            text_format_small,
            text_format_hud_title,
            text_format_hud_answer_big,
            text_format_hud_body,
            text_format_hud_small_left,
            render_target: None,
            screen_bitmap: None,
            brushes: None,
            phase: OverlayPhase::Selecting,
            start_instant: Instant::now(),
            ocr_rx: Some(rx),
            ocr_layout: OcrLayout::default(),
            line_capsules: Vec::new(),
            ocr_ready_instant: None,
            ocr_loading: true,
            ocr_error: None,
            selection_mode: default_mode,
            default_mode,
            is_dragging: false,
            drag_start: (init_x, init_y),
            drag_current: (init_x, init_y),
            hover_pos: (init_x, init_y),
            glow_pos: (init_x, init_y),
            current_selection: SelectionResult::default(),
            ai_rx: None,
            ai_loading: false,
            ai_start_instant: None,
            ai_answer: None,
            ai_error: None,
            captured_ocr_text: String::new(),
            hud_scroll_y: 0.0,
            hud_max_scroll_y: 0.0,
            hud_dragging_window: false,
            hud_drag_offset: (0, 0),
            status_toast: None,
            closing_soon: false,
        });

        let state_ptr = Box::into_raw(state);

        let hwnd = CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
            class_name,
            w!("AI-Task Overlay"),
            WS_POPUP,
            mon_x,
            mon_y,
            mon_w,
            mon_h,
            HWND::default(),
            None,
            hinstance,
            Some(state_ptr as *const _),
        )
        .map_err(|e| {
            let _ = Box::from_raw(state_ptr);
            format!("Gagal membuat window overlay AI-Task: {e}")
        })?;

        (*state_ptr).hwnd = hwnd;
        set_overlay_hwnd(hwnd);
        set_overlay_fullscreen_capturing(true);

        let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        let _ = SetTimer(hwnd, ANIM_TIMER_ID, ANIM_INTERVAL_MS, None);

        let mut msg = MSG::default();
        while GetMessageW(&mut msg, HWND::default(), 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
            if !IsWindow(hwnd).as_bool() {
                break;
            }
        }

        set_overlay_fullscreen_capturing(false);
        let final_state = Box::from_raw(state_ptr);
        crate::ui::trim_process_working_set();
        Ok(final_state.default_mode)
    }
}

unsafe extern "system" fn overlay_wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    use windows::Win32::UI::WindowsAndMessaging::{
        GetWindowLongPtrW, GetWindowRect, SetWindowLongPtrW, CREATESTRUCTW, GWLP_USERDATA,
        WM_NCCREATE,
    };

    if msg == WM_NCCREATE {
        let cs = lparam.0 as *const CREATESTRUCTW;
        if !cs.is_null() {
            let state_ptr = (*cs).lpCreateParams as isize;
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, state_ptr);
        }
        return DefWindowProcW(hwnd, msg, wparam, lparam);
    }

    let state_ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut OverlayState;
    if state_ptr.is_null() {
        return DefWindowProcW(hwnd, msg, wparam, lparam);
    }
    let state = &mut *state_ptr;

    match msg {
        WM_ERASEBKGND => LRESULT(1),

        WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),

        WM_APP_OVERLAY_COPY => {
            if state.phase == OverlayPhase::Answering {
                if let Some(ans) = &state.ai_answer {
                    let _ = copy_text_to_clipboard(&ans.copy_payload);
                    state.status_toast = Some((
                        "Jawaban disalin ke Clipboard".to_string(),
                        Instant::now(),
                    ));
                    let _ = InvalidateRect(hwnd, None, false);
                }
            } else if !state.current_selection.text.is_empty() {
                let chosen = state.current_selection.text.clone();
                state.finish_copy_and_close(&chosen);
            }
            LRESULT(0)
        }

        WM_TIMER => {
            if wparam.0 == CLOSE_DELAY_TIMER_ID {
                let _ = KillTimer(hwnd, CLOSE_DELAY_TIMER_ID);
                let _ = DestroyWindow(hwnd);
            } else if wparam.0 == ANIM_TIMER_ID {
                state.poll_ocr_result();
                state.poll_ai_result();
                if state.needs_animation_tick() {
                    let _ = InvalidateRect(hwnd, None, false);
                }
            }
            LRESULT(0)
        }

        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            let _ = BeginPaint(hwnd, &mut ps);
            state.render();
            let _ = EndPaint(hwnd, &ps);
            LRESULT(0)
        }

        WM_SETCURSOR => {
            let (hx, hy) = state.hover_pos;
            let cursor_id = if state.phase == OverlayPhase::Answering {
                if state.hud_close_btn_rect().contains(hx, hy)
                    || state.hud_btn_copy_answer_rect().contains(hx, hy)
                    || state.hud_btn_copy_ocr_rect().contains(hx, hy)
                    || state.hud_btn_scroll_up_rect().contains(hx, hy)
                    || state.hud_btn_scroll_down_rect().contains(hx, hy)
                    || state.hud_btn_dismiss_rect().contains(hx, hy)
                {
                    IDC_HAND
                } else if state.hud_header_rect().contains(hx, hy) {
                    IDC_SIZEALL
                } else {
                    IDC_ARROW
                }
            } else if state.is_point_on_toolbar(hx, hy) {
                IDC_HAND
            } else if state.effective_mode() == SelectionMode::Box {
                IDC_CROSS
            } else if state.ocr_layout.word_at_point(hx, hy).is_some() {
                IDC_IBEAM
            } else {
                IDC_ARROW
            };
            if let Ok(cursor) = LoadCursorW(None, cursor_id) {
                SetCursor(cursor);
            }
            LRESULT(1)
        }

        WM_MOUSEWHEEL => {
            if state.phase == OverlayPhase::Answering {
                let delta = ((wparam.0 >> 16) as i16) as f32;
                let step = if delta > 0.0 { -40.0 } else { 40.0 };
                state.hud_scroll_y = (state.hud_scroll_y + step).clamp(0.0, state.hud_max_scroll_y);
                let _ = InvalidateRect(hwnd, None, false);
            }
            LRESULT(0)
        }

        WM_MOUSEMOVE => {
            let x = (lparam.0 & 0xFFFF) as i16 as f32;
            let y = ((lparam.0 >> 16) & 0xFFFF) as i16 as f32;

            if state.phase == OverlayPhase::Answering && state.hud_dragging_window {
                let mut pt = POINT::default();
                if GetCursorPos(&mut pt).is_ok() {
                    let new_x = pt.x - state.hud_drag_offset.0;
                    let new_y = pt.y - state.hud_drag_offset.1;
                    let _ = SetWindowPos(
                        hwnd,
                        HWND_TOPMOST,
                        new_x,
                        new_y,
                        HUD_WIDTH as i32,
                        HUD_HEIGHT as i32,
                        SWP_NOACTIVATE,
                    );
                }
                return LRESULT(0);
            }

            if (x - state.hover_pos.0).abs() > 0.5 || (y - state.hover_pos.1).abs() > 0.5 {
                state.hover_pos = (x, y);
                if state.phase == OverlayPhase::Selecting && state.is_dragging && !state.closing_soon
                {
                    state.drag_current = (x, y);
                    state.update_selection();
                }
                let _ = InvalidateRect(hwnd, None, false);
            }
            LRESULT(0)
        }

        WM_LBUTTONDOWN => {
            if state.closing_soon {
                return LRESULT(0);
            }
            let x = (lparam.0 & 0xFFFF) as i16 as f32;
            let y = ((lparam.0 >> 16) & 0xFFFF) as i16 as f32;
            state.hover_pos = (x, y);

            if state.phase == OverlayPhase::Answering {
                if state.hud_close_btn_rect().contains(x, y)
                    || state.hud_btn_dismiss_rect().contains(x, y)
                {
                    let _ = DestroyWindow(hwnd);
                    return LRESULT(0);
                }
                if state.hud_btn_copy_answer_rect().contains(x, y) {
                    if let Some(ans) = &state.ai_answer {
                        let _ = copy_text_to_clipboard(&ans.copy_payload);
                        state.status_toast = Some((
                            "Jawaban disalin ke Clipboard".to_string(),
                            Instant::now(),
                        ));
                        let _ = InvalidateRect(hwnd, None, false);
                    }
                    return LRESULT(0);
                }
                if state.hud_btn_copy_ocr_rect().contains(x, y) {
                    if !state.captured_ocr_text.trim().is_empty() {
                        let _ = copy_text_to_clipboard(&state.captured_ocr_text);
                        state.status_toast = Some((
                            "Teks soal disalin ke Clipboard".to_string(),
                            Instant::now(),
                        ));
                        let _ = InvalidateRect(hwnd, None, false);
                    }
                    return LRESULT(0);
                }
                if state.hud_btn_scroll_up_rect().contains(x, y) {
                    state.hud_scroll_y = (state.hud_scroll_y - 45.0).max(0.0);
                    let _ = InvalidateRect(hwnd, None, false);
                    return LRESULT(0);
                }
                if state.hud_btn_scroll_down_rect().contains(x, y) {
                    state.hud_scroll_y =
                        (state.hud_scroll_y + 45.0).min(state.hud_max_scroll_y);
                    let _ = InvalidateRect(hwnd, None, false);
                    return LRESULT(0);
                }
                if state.hud_header_rect().contains(x, y) {
                    let mut pt = POINT::default();
                    let mut rc = windows::Win32::Foundation::RECT::default();
                    if GetCursorPos(&mut pt).is_ok() && GetWindowRect(hwnd, &mut rc).is_ok() {
                        state.hud_dragging_window = true;
                        state.hud_drag_offset = (pt.x - rc.left, pt.y - rc.top);
                        let _ = SetCapture(hwnd);
                    }
                    return LRESULT(0);
                }
                return LRESULT(0);
            }

            if state.btn_box_rect().contains(x, y) {
                state.selection_mode = SelectionMode::Box;
                state.default_mode = SelectionMode::Box;
                let _ = InvalidateRect(hwnd, None, false);
                return LRESULT(0);
            }
            if state.btn_linear_rect().contains(x, y) {
                state.selection_mode = SelectionMode::Linear;
                state.default_mode = SelectionMode::Linear;
                let _ = InvalidateRect(hwnd, None, false);
                return LRESULT(0);
            }
            if state.btn_solve_all_rect().contains(x, y) {
                let full_rect = RectF::new(0.0, 0.0, state.width_f(), state.height_f());
                let all_text = state.ocr_layout.full_text();
                state.start_ai_solving(full_rect, all_text);
                return LRESULT(0);
            }
            if state.btn_copy_all_rect().contains(x, y) {
                let all_text = state.ocr_layout.full_text();
                if !all_text.is_empty() {
                    state.finish_copy_and_close(&all_text);
                } else {
                    state.status_toast =
                        Some(("Belum ada teks untuk disalin.".to_string(), Instant::now()));
                }
                return LRESULT(0);
            }
            if state.btn_close_rect().contains(x, y) {
                let _ = DestroyWindow(hwnd);
                return LRESULT(0);
            }

            state.is_dragging = true;
            state.drag_start = (x, y);
            state.drag_current = (x, y);
            state.update_selection();
            let _ = SetCapture(hwnd);
            let _ = InvalidateRect(hwnd, None, false);
            LRESULT(0)
        }

        WM_LBUTTONUP => {
            if state.phase == OverlayPhase::Answering {
                if state.hud_dragging_window {
                    state.hud_dragging_window = false;
                    let _ = ReleaseCapture();
                }
                return LRESULT(0);
            }

            if state.is_dragging {
                state.is_dragging = false;
                let _ = ReleaseCapture();

                let x = (lparam.0 & 0xFFFF) as i16 as f32;
                let y = ((lparam.0 >> 16) & 0xFFFF) as i16 as f32;
                state.drag_current = (x, y);
                state.update_selection();

                if state.current_selection.has_valid_target() {
                    let crop = state.current_selection.crop_bounds.unwrap_or_else(|| {
                        RectF::from_points(
                            state.drag_start.0,
                            state.drag_start.1,
                            state.drag_current.0,
                            state.drag_current.1,
                        )
                    });
                    let ocr_txt = state.current_selection.text.clone();
                    state.start_ai_solving(crop, ocr_txt);
                } else {
                    let _ = InvalidateRect(hwnd, None, false);
                }
            }
            LRESULT(0)
        }

        WM_RBUTTONUP => {
            let _ = DestroyWindow(hwnd);
            LRESULT(0)
        }

        WM_KEYDOWN => {
            let vk = wparam.0 as u16;
            if vk == VK_ESCAPE.0 {
                let _ = DestroyWindow(hwnd);
            } else if vk == b'C' as u16 {
                let ctrl = (GetAsyncKeyState(VK_CONTROL.0 as i32) as u16 & 0x8000) != 0;
                if ctrl {
                    if state.phase == OverlayPhase::Answering {
                        if let Some(ans) = &state.ai_answer {
                            let _ = copy_text_to_clipboard(&ans.copy_payload);
                        }
                    } else if !state.current_selection.text.is_empty() {
                        let chosen = state.current_selection.text.clone();
                        state.finish_copy_and_close(&chosen);
                    }
                }
            }
            LRESULT(0)
        }

        WM_CLOSE => {
            let _ = DestroyWindow(hwnd);
            LRESULT(0)
        }

        WM_DESTROY => {
            let _ = KillTimer(hwnd, ANIM_TIMER_ID);
            let _ = KillTimer(hwnd, CLOSE_DELAY_TIMER_ID);
            set_overlay_fullscreen_capturing(false);
            state.brushes = None;
            state.screen_bitmap = None;
            state.render_target = None;
            state.frame.pixels_bgra.clear();
            state.frame.pixels_bgra.shrink_to_fit();
            LRESULT(0)
        }

        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_multiple_choice_option_letters_reliably() {
        assert_eq!(
            extract_option_letter(
                "B. Tingginya angka sentimen positif berbanding terbalik dengan kualitas literasi kritis"
            ),
            Some('B')
        );
        assert_eq!(extract_option_letter("A) Mayoritas remaja"), Some('A'));
        assert_eq!(extract_option_letter("[D] Lonjakan perbincangan"), Some('D'));
        assert_eq!(extract_option_letter("Opsi C: Sebanyak 10% remaja"), Some('C'));
        assert_eq!(
            extract_option_letter("Berdasarkan analisis data, kesimpulannya adalah..."),
            None
        );
    }
}
