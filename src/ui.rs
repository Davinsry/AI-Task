use crate::ai::{fetch_provider_models, AiConfig, AiProvider};
use crate::clipboard::read_text_from_clipboard;
use crate::hook::{get_active_shortcut, save_shortcut, ShortcutConfig, TriggerInput};
use crate::selection::{RectF, SelectionMode};
use crate::tray::{
    create_aitask_logo_icon, ensure_default_autostart, is_autostart_enabled, set_autostart_enabled,
};
use std::ffi::c_void;
use std::sync::mpsc::{self, Receiver};
use std::thread;
use windows::core::w;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::Graphics::Direct2D::Common::{
    D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_COLOR_F, D2D1_PIXEL_FORMAT, D2D_POINT_2F, D2D_RECT_F,
    D2D_SIZE_U,
};
use windows::Win32::Graphics::Direct2D::{
    D2D1CreateFactory, ID2D1Factory, ID2D1HwndRenderTarget, ID2D1SolidColorBrush,
    D2D1_DRAW_TEXT_OPTIONS_CLIP, D2D1_FACTORY_TYPE_SINGLE_THREADED,
    D2D1_HWND_RENDER_TARGET_PROPERTIES, D2D1_PRESENT_OPTIONS_NONE, D2D1_RENDER_TARGET_PROPERTIES,
    D2D1_RENDER_TARGET_TYPE_DEFAULT, D2D1_RENDER_TARGET_USAGE_NONE, D2D1_ROUNDED_RECT,
};
use windows::Win32::Graphics::DirectWrite::{
    DWriteCreateFactory, IDWriteFactory, IDWriteTextFormat, DWRITE_FACTORY_TYPE_SHARED,
    DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_WEIGHT_BOLD,
    DWRITE_FONT_WEIGHT_NORMAL, DWRITE_FONT_WEIGHT_SEMI_BOLD, DWRITE_MEASURING_MODE_NATURAL,
    DWRITE_PARAGRAPH_ALIGNMENT_CENTER, DWRITE_TEXT_ALIGNMENT_CENTER, DWRITE_TEXT_ALIGNMENT_LEADING,
    DWRITE_TEXT_ALIGNMENT_TRAILING,
};
use windows::Win32::Graphics::Dwm::{
    DwmSetWindowAttribute, DWMWA_USE_IMMERSIVE_DARK_MODE, DWMWA_WINDOW_CORNER_PREFERENCE,
    DWMWCP_ROUND,
};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::Graphics::Gdi::{BeginPaint, EndPaint, InvalidateRect, PAINTSTRUCT};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::{AdjustWindowRectExForDpi, GetDpiForWindow};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, VK_CONTROL, VK_ESCAPE, VK_LCONTROL, VK_LMENU, VK_LSHIFT, VK_LWIN, VK_MENU,
    VK_RCONTROL, VK_RMENU, VK_RSHIFT, VK_RWIN, VK_SHIFT,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyIcon, DestroyWindow, DispatchMessageW, GetClientRect,
    GetMessageW, GetSystemMetrics, GetWindowLongPtrW, IsWindow, KillTimer, LoadCursorW,
    RegisterClassW, SendMessageW, SetCursor, SetForegroundWindow, SetTimer, SetWindowLongPtrW,
    SetWindowPos, ShowWindow, TranslateMessage, CREATESTRUCTW, CS_HREDRAW, CS_VREDRAW,
    GWLP_USERDATA, HICON, HWND_TOP, ICON_BIG, ICON_SMALL, IDC_ARROW, IDC_HAND, IDC_IBEAM, MSG,
    SM_CXSCREEN, SM_CYSCREEN, SWP_NOZORDER, SW_SHOW, WINDOW_EX_STYLE, WM_CHAR, WM_CLOSE,
    WM_DESTROY, WM_ERASEBKGND, WM_KEYDOWN, WM_LBUTTONDOWN, WM_MBUTTONDOWN, WM_MOUSEMOVE,
    WM_MOUSEWHEEL, WM_NCCREATE, WM_PAINT, WM_RBUTTONDOWN, WM_SETCURSOR, WM_SETICON, WM_SIZE,
    WM_SYSKEYDOWN, WM_TIMER, WM_XBUTTONDOWN, WNDCLASSW, WS_CAPTION, WS_MINIMIZEBOX, WS_OVERLAPPED,
    WS_SYSMENU, WS_VISIBLE,
};

#[link(name = "kernel32")]
extern "system" {
    fn GetCurrentProcess() -> isize;
    fn SetProcessWorkingSetSize(hprocess: isize, min: usize, max: usize) -> i32;
}

pub fn trim_process_working_set() {
    unsafe {
        let _ = SetProcessWorkingSetSize(GetCurrentProcess(), usize::MAX, usize::MAX);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DashboardAction {
    MinimizeToTray(SelectionMode),
    TriggerScanNow(SelectionMode),
    ExitApplication,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ActiveTextInput {
    None,
    ApiKey,
    BaseUrl,
    ModelManual,
}

const WIN_WIDTH: u32 = 560;
const WIN_HEIGHT: u32 = 748;
const FETCH_POLL_TIMER_ID: usize = 201;
const MODELS_PER_PAGE: usize = 6;

struct DashboardState {
    hwnd: HWND,
    d2d_factory: ID2D1Factory,
    _dwrite_factory: IDWriteFactory,
    fmt_title: IDWriteTextFormat,
    fmt_section: IDWriteTextFormat,
    fmt_body_left: IDWriteTextFormat,
    fmt_body_center: IDWriteTextFormat,
    fmt_small_left: IDWriteTextFormat,
    fmt_small_right: IDWriteTextFormat,
    fmt_small_center: IDWriteTextFormat,
    rt: Option<ID2D1HwndRenderTarget>,
    ui_scale: f32,

    // AI Configuration State
    ai_config: AiConfig,
    active_input: ActiveTextInput,
    show_api_key: bool,
    fetch_rx: Option<Receiver<Result<Vec<String>, String>>>,
    fetching_models: bool,
    fetch_status_msg: String,
    fetch_is_error: bool,
    model_page: usize,

    // Shortcut & System State
    active_shortcut: ShortcutConfig,
    recording_custom: bool,
    selection_mode: SelectionMode,
    autostart: bool,

    hover: (f32, f32),
    final_action: DashboardAction,
}

impl DashboardState {
    // ─────────────────────────────────────────────────────────────
    // Header & AI Provider Section Coordinates
    // ─────────────────────────────────────────────────────────────
    fn btn_scan_now_rect(&self) -> RectF {
        RectF::new(380.0, 20.0, 156.0, 34.0)
    }

    fn provider_tab_rect(&self, index: usize) -> RectF {
        let w = 96.0;
        let gap = 8.0;
        let x = 24.0 + (index as f32) * (w + gap);
        RectF::new(x, 96.0, w, 30.0)
    }

    fn input_api_key_rect(&self) -> RectF {
        RectF::new(24.0, 134.0, 332.0, 32.0)
    }

    fn btn_paste_key_rect(&self) -> RectF {
        RectF::new(362.0, 134.0, 82.0, 32.0)
    }

    fn btn_toggle_show_key_rect(&self) -> RectF {
        RectF::new(450.0, 134.0, 86.0, 32.0)
    }

    fn input_base_url_rect(&self) -> RectF {
        RectF::new(24.0, 172.0, 332.0, 30.0)
    }

    fn btn_paste_url_rect(&self) -> RectF {
        RectF::new(362.0, 172.0, 82.0, 30.0)
    }

    fn btn_fetch_models_rect(&self) -> RectF {
        if self.ai_config.provider == AiProvider::Custom {
            RectF::new(450.0, 172.0, 86.0, 30.0)
        } else {
            RectF::new(362.0, 172.0, 174.0, 30.0)
        }
    }

    fn input_model_manual_rect(&self) -> RectF {
        RectF::new(24.0, 210.0, 332.0, 32.0)
    }

    fn btn_paste_model_rect(&self) -> RectF {
        RectF::new(362.0, 210.0, 82.0, 32.0)
    }

    fn btn_prev_model_page_rect(&self) -> RectF {
        RectF::new(450.0, 210.0, 40.0, 32.0)
    }

    fn btn_next_model_page_rect(&self) -> RectF {
        RectF::new(496.0, 210.0, 40.0, 32.0)
    }

    fn fetched_model_card_rect(&self, slot: usize) -> RectF {
        let col = slot % 2;
        let row = slot / 2;
        let x = if col == 0 { 24.0 } else { 284.0 };
        let y = 248.0 + (row as f32) * 34.0;
        RectF::new(x, y, 252.0, 28.0)
    }

    // ─────────────────────────────────────────────────────────────
    // Shortcut Section Coordinates (y: 382..540)
    // ─────────────────────────────────────────────────────────────
    fn active_shortcut_bar_rect(&self) -> RectF {
        RectF::new(24.0, 406.0, 366.0, 32.0)
    }

    fn record_custom_btn_rect(&self) -> RectF {
        RectF::new(398.0, 406.0, 138.0, 32.0)
    }

    fn preset_card_rect(&self, index: usize) -> RectF {
        let col = index % 2;
        let row = index / 2;
        let x = if col == 0 { 24.0 } else { 284.0 };
        let y = 446.0 + (row as f32) * 34.0;
        RectF::new(x, y, 252.0, 28.0)
    }

    // ─────────────────────────────────────────────────────────────
    // Mode & Toggles Section Coordinates (y: 558..680)
    // ─────────────────────────────────────────────────────────────
    fn mode_box_rect(&self) -> RectF {
        RectF::new(24.0, 580.0, 252.0, 46.0)
    }

    fn mode_linear_rect(&self) -> RectF {
        RectF::new(284.0, 580.0, 252.0, 46.0)
    }

    fn autocopy_toggle_rect(&self) -> RectF {
        RectF::new(24.0, 634.0, 252.0, 42.0)
    }

    fn autostart_toggle_rect(&self) -> RectF {
        RectF::new(284.0, 634.0, 252.0, 42.0)
    }

    // ─────────────────────────────────────────────────────────────
    // Footer Actions (y: 694)
    // ─────────────────────────────────────────────────────────────
    fn btn_hide_tray_rect(&self) -> RectF {
        RectF::new(24.0, 692.0, 322.0, 36.0)
    }

    fn btn_exit_app_rect(&self) -> RectF {
        RectF::new(356.0, 692.0, 180.0, 36.0)
    }

    fn total_model_pages(&self) -> usize {
        let len = self.ai_config.fetched_models.len();
        if len == 0 {
            1
        } else {
            (len + MODELS_PER_PAGE - 1) / MODELS_PER_PAGE
        }
    }

    fn switch_provider(&mut self, next_provider: AiProvider) {
        let _ = self.ai_config.save();
        self.ai_config = AiConfig::load_for_provider(next_provider);
        let _ = self.ai_config.save();
        self.model_page = 0;
        self.fetching_models = false;
        self.fetch_rx = None;
        self.fetch_is_error = false;
        self.fetch_status_msg = if self.ai_config.fetched_models.is_empty() {
            format!(
                "Provider {} dipilih. Isi API Key lalu klik 'Muat Model'.",
                next_provider.display_name()
            )
        } else {
            format!(
                "{} model {} tersimpan siap dipilih.",
                self.ai_config.fetched_models.len(),
                next_provider.display_name()
            )
        };
    }

    fn trigger_fetch_models(&mut self) {
        if self.fetching_models {
            return;
        }
        let _ = self.ai_config.save();
        let cfg = self.ai_config.clone();
        self.fetching_models = true;
        self.fetch_is_error = false;
        self.fetch_status_msg = format!(
            "Menembak endpoint /models {}...",
            cfg.provider.display_name()
        );

        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            let res = fetch_provider_models(&cfg);
            let _ = tx.send(res);
        });
        self.fetch_rx = Some(rx);

        unsafe {
            let _ = SetTimer(self.hwnd, FETCH_POLL_TIMER_ID, 60, None);
            let _ = InvalidateRect(self.hwnd, None, false);
        }
    }

    fn poll_fetch_models(&mut self) {
        if let Some(rx) = &self.fetch_rx {
            if let Ok(res) = rx.try_recv() {
                self.fetching_models = false;
                self.fetch_rx = None;
                unsafe {
                    let _ = KillTimer(self.hwnd, FETCH_POLL_TIMER_ID);
                }
                match res {
                    Ok(models) => {
                        let count = models.len();
                        if self.ai_config.selected_model.trim().is_empty()
                            || !models.contains(&self.ai_config.selected_model)
                        {
                            if let Some(first) = models.first() {
                                self.ai_config.selected_model = first.clone();
                            }
                        }
                        self.ai_config.fetched_models = models;
                        self.model_page = 0;
                        self.fetch_is_error = false;
                        self.fetch_status_msg = format!(
                            "Berhasil memuat {count} model dari API {}. Klik model di bawah untuk memilih.",
                            self.ai_config.provider.display_name()
                        );
                        let _ = self.ai_config.save();
                    }
                    Err(err) => {
                        self.fetch_is_error = true;
                        self.fetch_status_msg = err;
                    }
                }
                unsafe {
                    let _ = InvalidateRect(self.hwnd, None, false);
                }
            }
        }
    }

    fn is_interactive_at(&self, x: f32, y: f32) -> bool {
        if self.btn_scan_now_rect().contains(x, y)
            || self.input_api_key_rect().contains(x, y)
            || self.btn_paste_key_rect().contains(x, y)
            || self.btn_toggle_show_key_rect().contains(x, y)
            || self.btn_fetch_models_rect().contains(x, y)
            || self.input_model_manual_rect().contains(x, y)
            || self.btn_paste_model_rect().contains(x, y)
            || self.btn_prev_model_page_rect().contains(x, y)
            || self.btn_next_model_page_rect().contains(x, y)
            || self.record_custom_btn_rect().contains(x, y)
            || self.mode_box_rect().contains(x, y)
            || self.mode_linear_rect().contains(x, y)
            || self.autocopy_toggle_rect().contains(x, y)
            || self.autostart_toggle_rect().contains(x, y)
            || self.btn_hide_tray_rect().contains(x, y)
            || self.btn_exit_app_rect().contains(x, y)
        {
            return true;
        }
        if self.ai_config.provider == AiProvider::Custom
            && (self.input_base_url_rect().contains(x, y)
                || self.btn_paste_url_rect().contains(x, y))
        {
            return true;
        }
        for i in 0..5 {
            if self.provider_tab_rect(i).contains(x, y) {
                return true;
            }
        }
        for i in 0..MODELS_PER_PAGE {
            let idx = self.model_page * MODELS_PER_PAGE + i;
            if idx < self.ai_config.fetched_models.len()
                && self.fetched_model_card_rect(i).contains(x, y)
            {
                return true;
            }
        }
        for i in 0..6 {
            if self.preset_card_rect(i).contains(x, y) {
                return true;
            }
        }
        false
    }

    fn is_text_input_at(&self, x: f32, y: f32) -> bool {
        self.input_api_key_rect().contains(x, y)
            || self.input_model_manual_rect().contains(x, y)
            || (self.ai_config.provider == AiProvider::Custom
                && self.input_base_url_rect().contains(x, y))
    }

    unsafe fn ensure_rt(&mut self) {
        if self.rt.is_some() {
            return;
        }
        let mut rc = windows::Win32::Foundation::RECT::default();
        let _ = GetClientRect(self.hwnd, &mut rc);
        let w = (rc.right - rc.left).max(1) as u32;
        let h = (rc.bottom - rc.top).max(1) as u32;
        let dpi = 96.0 * self.ui_scale;

        let props = D2D1_RENDER_TARGET_PROPERTIES {
            r#type: D2D1_RENDER_TARGET_TYPE_DEFAULT,
            pixelFormat: D2D1_PIXEL_FORMAT {
                format: DXGI_FORMAT_B8G8R8A8_UNORM,
                alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
            },
            dpiX: dpi,
            dpiY: dpi,
            usage: D2D1_RENDER_TARGET_USAGE_NONE,
            minLevel: Default::default(),
        };
        let hwnd_props = D2D1_HWND_RENDER_TARGET_PROPERTIES {
            hwnd: self.hwnd,
            pixelSize: D2D_SIZE_U {
                width: w,
                height: h,
            },
            presentOptions: D2D1_PRESENT_OPTIONS_NONE,
        };
        if let Ok(rt) = self.d2d_factory.CreateHwndRenderTarget(&props, &hwnd_props) {
            self.rt = Some(rt);
        }
    }

    unsafe fn render(&mut self) {
        self.ensure_rt();
        let Some(rt) = self.rt.clone() else { return };

        let (hx, hy) = self.hover;

        rt.BeginDraw();
        // Base canvas: exact #111111 matte neutral carbon
        rt.Clear(Some(&D2D1_COLOR_F {
            r: 0.067,
            g: 0.067,
            b: 0.067,
            a: 1.0,
        }));

        let surface_bg = brush(&rt, 0.106, 0.106, 0.106, 1.0); // #1B1B1B
        let surface_hover = brush(&rt, 0.145, 0.145, 0.145, 1.0); // #252525
        let surface_selected = brush(&rt, 0.137, 0.137, 0.137, 1.0); // #232323
        let border_subtle = brush(&rt, 0.176, 0.176, 0.176, 1.0); // #2D2D2D
        let divider_line = brush(&rt, 0.149, 0.149, 0.149, 1.0); // #262626
        let border_active = brush(&rt, 0.929, 0.929, 0.929, 1.0); // #EDEDED
        let border_input_focus = brush(&rt, 0.220, 0.741, 0.973, 1.0); // #38BDF8
        let cta_primary_bg = brush(&rt, 0.949, 0.949, 0.949, 1.0); // #F2F2F2
        let cta_primary_hover = brush(&rt, 0.850, 0.850, 0.850, 1.0); // #D9D9D9
        let text_dark = brush(&rt, 0.067, 0.067, 0.067, 1.0); // #111111
        let text_primary = brush(&rt, 0.949, 0.949, 0.949, 1.0); // #F2F2F2
        let text_secondary = brush(&rt, 0.635, 0.635, 0.635, 1.0); // #A2A2A2
        let switch_on_green = brush(&rt, 0.133, 0.773, 0.369, 1.0); // #22C55E
        let danger_surface = brush(&rt, 0.160, 0.090, 0.090, 1.0);
        let danger_border = brush(&rt, 0.520, 0.200, 0.200, 1.0);
        let text_error = brush(&rt, 0.960, 0.420, 0.420, 1.0);

        // ─────────────────────────────────────────────────────────────
        // 1. TOP HEADER
        // ─────────────────────────────────────────────────────────────
        draw_aitask_header_logo(&rt, RectF::new(24.0, 14.0, 44.0, 44.0));

        if let Some(tp) = &text_primary {
            draw_text(
                &rt,
                &self.fmt_title,
                "AI-Task",
                RectF::new(78.0, 14.0, 220.0, 24.0),
                tp,
            );
        }
        if let Some(ts) = &text_secondary {
            let ready_tag = if self.ai_config.is_ready() {
                format!(
                    "Siap ({})  ·  Stealth Tray",
                    self.ai_config.selected_model
                )
            } else {
                "Stealth Question Solver  ·  Aktif di System Tray".to_string()
            };
            draw_text(
                &rt,
                &self.fmt_small_left,
                &ready_tag,
                RectF::new(78.0, 39.0, 294.0, 18.0),
                ts,
            );
        }

        let scan_r = self.btn_scan_now_rect();
        if scan_r.contains(hx, hy) {
            fill_rrect(&rt, scan_r, 6.0, &cta_primary_hover);
        } else {
            fill_rrect(&rt, scan_r, 6.0, &cta_primary_bg);
        }
        if let Some(td) = &text_dark {
            draw_text(&rt, &self.fmt_body_center, "Blok Soal Sekarang", scan_r, td);
        }

        draw_hline(&rt, 24.0, 536.0, 68.0, &divider_line);

        // ─────────────────────────────────────────────────────────────
        // 2. KONFIGURASI PROVIDER AI, API KEY, & PEMILIH MODEL DINAMIS
        // ─────────────────────────────────────────────────────────────
        if let Some(ts) = &text_secondary {
            draw_text(
                &rt,
                &self.fmt_section,
                "Provider AI, API Key, & Pemilihan Model",
                RectF::new(24.0, 74.0, 340.0, 18.0),
                ts,
            );
        }

        // 5 Provider Tabs
        let providers = AiProvider::all();
        for (i, prov) in providers.iter().copied().enumerate() {
            let tab_r = self.provider_tab_rect(i);
            let is_active = self.ai_config.provider == prov;
            if is_active {
                fill_rrect(&rt, tab_r, 6.0, &surface_selected);
                stroke_rrect(&rt, tab_r, 6.0, 1.5, &border_active);
            } else if tab_r.contains(hx, hy) {
                fill_rrect(&rt, tab_r, 6.0, &surface_hover);
                stroke_rrect(&rt, tab_r, 6.0, 1.0, &border_subtle);
            } else {
                fill_rrect(&rt, tab_r, 6.0, &surface_bg);
                stroke_rrect(&rt, tab_r, 6.0, 1.0, &border_subtle);
            }
            let col = if is_active {
                &text_primary
            } else {
                &text_secondary
            };
            if let Some(b) = col {
                draw_text(&rt, &self.fmt_small_center, prov.display_name(), tab_r, b);
            }
        }

        // API Key Input Row
        let key_r = self.input_api_key_rect();
        fill_rrect(&rt, key_r, 6.0, &surface_bg);
        if self.active_input == ActiveTextInput::ApiKey {
            stroke_rrect(&rt, key_r, 6.0, 1.5, &border_input_focus);
        } else {
            stroke_rrect(&rt, key_r, 6.0, 1.0, &border_subtle);
        }

        let key_trimmed = self.ai_config.api_key.trim();
        let (key_display, key_is_placeholder) = if key_trimmed.is_empty() {
            (
                format!(
                    "Klik lalu ketik / Ctrl+V API Key {}...",
                    self.ai_config.provider.display_name()
                ),
                true,
            )
        } else if self.show_api_key {
            (key_trimmed.to_string(), false)
        } else {
            let chars: Vec<char> = key_trimmed.chars().collect();
            if chars.len() <= 8 {
                ("*".repeat(chars.len()), false)
            } else {
                let prefix: String = chars[..4].iter().collect();
                let suffix: String = chars[chars.len() - 4..].iter().collect();
                (format!("{prefix}••••••••••••{suffix}"), false)
            }
        };

        let key_brush = if key_is_placeholder {
            &text_secondary
        } else {
            &text_primary
        };
        if let Some(b) = key_brush {
            draw_text(
                &rt,
                &self.fmt_small_left,
                &key_display,
                RectF::new(key_r.x + 10.0, key_r.y, key_r.width - 20.0, key_r.height),
                b,
            );
        }

        // Paste Key & Show/Hide Key buttons
        let paste_key_r = self.btn_paste_key_rect();
        if paste_key_r.contains(hx, hy) {
            fill_rrect(&rt, paste_key_r, 6.0, &surface_hover);
        } else {
            fill_rrect(&rt, paste_key_r, 6.0, &surface_bg);
        }
        stroke_rrect(&rt, paste_key_r, 6.0, 1.0, &border_subtle);
        if let Some(tp) = &text_primary {
            draw_text(&rt, &self.fmt_small_center, "Tempel Key", paste_key_r, tp);
        }

        let show_key_r = self.btn_toggle_show_key_rect();
        if show_key_r.contains(hx, hy) {
            fill_rrect(&rt, show_key_r, 6.0, &surface_hover);
        } else {
            fill_rrect(&rt, show_key_r, 6.0, &surface_bg);
        }
        stroke_rrect(&rt, show_key_r, 6.0, 1.0, &border_subtle);
        if let Some(ts) = &text_secondary {
            let lbl = if self.show_api_key {
                "Sembunyi"
            } else {
                "Lihat Key"
            };
            draw_text(&rt, &self.fmt_small_center, lbl, show_key_r, ts);
        }

        // Endpoint URL Row + "Tembak API & Muat Model" button
        let url_r = self.input_base_url_rect();
        fill_rrect(&rt, url_r, 6.0, &surface_bg);
        if self.ai_config.provider == AiProvider::Custom
            && self.active_input == ActiveTextInput::BaseUrl
        {
            stroke_rrect(&rt, url_r, 6.0, 1.5, &border_input_focus);
        } else {
            stroke_rrect(&rt, url_r, 6.0, 1.0, &border_subtle);
        }
        let endpoint_text = format!("Endpoint: {}", self.ai_config.effective_base_url());
        if let Some(ts) = &text_secondary {
            draw_text(
                &rt,
                &self.fmt_small_left,
                &endpoint_text,
                RectF::new(url_r.x + 10.0, url_r.y, url_r.width - 20.0, url_r.height),
                ts,
            );
        }

        if self.ai_config.provider == AiProvider::Custom {
            let purl_r = self.btn_paste_url_rect();
            if purl_r.contains(hx, hy) {
                fill_rrect(&rt, purl_r, 6.0, &surface_hover);
            } else {
                fill_rrect(&rt, purl_r, 6.0, &surface_bg);
            }
            stroke_rrect(&rt, purl_r, 6.0, 1.0, &border_subtle);
            if let Some(tp) = &text_primary {
                draw_text(&rt, &self.fmt_small_center, "Tempel URL", purl_r, tp);
            }
        }

        let fetch_r = self.btn_fetch_models_rect();
        if fetch_r.contains(hx, hy) {
            fill_rrect(&rt, fetch_r, 6.0, &cta_primary_hover);
        } else {
            fill_rrect(&rt, fetch_r, 6.0, &cta_primary_bg);
        }
        if let Some(td) = &text_dark {
            let fetch_lbl = if self.fetching_models {
                "Memuat..."
            } else if self.ai_config.provider == AiProvider::Custom {
                "Muat Model"
            } else {
                "Tembak API & Muat Model"
            };
            draw_text(&rt, &self.fmt_small_center, fetch_lbl, fetch_r, td);
        }

        // Manual Model Input + Pagination Row
        let mod_r = self.input_model_manual_rect();
        fill_rrect(&rt, mod_r, 6.0, &surface_bg);
        if self.active_input == ActiveTextInput::ModelManual {
            stroke_rrect(&rt, mod_r, 6.0, 1.5, &border_input_focus);
        } else {
            stroke_rrect(&rt, mod_r, 6.0, 1.0, &border_subtle);
        }

        let mod_trimmed = self.ai_config.selected_model.trim();
        if mod_trimmed.is_empty() {
            if let Some(ts) = &text_secondary {
                draw_text(
                    &rt,
                    &self.fmt_small_left,
                    "Model Aktif: (Ketik manual atau klik daftar model di bawah)",
                    RectF::new(mod_r.x + 10.0, mod_r.y, mod_r.width - 20.0, mod_r.height),
                    ts,
                );
            }
        } else if let Some(tp) = &text_primary {
            draw_text(
                &rt,
                &self.fmt_body_left,
                &format!("Model: {mod_trimmed}"),
                RectF::new(mod_r.x + 10.0, mod_r.y, mod_r.width - 20.0, mod_r.height),
                tp,
            );
        }

        let pmod_r = self.btn_paste_model_rect();
        if pmod_r.contains(hx, hy) {
            fill_rrect(&rt, pmod_r, 6.0, &surface_hover);
        } else {
            fill_rrect(&rt, pmod_r, 6.0, &surface_bg);
        }
        stroke_rrect(&rt, pmod_r, 6.0, 1.0, &border_subtle);
        if let Some(tp) = &text_primary {
            draw_text(&rt, &self.fmt_small_center, "Tempel Model", pmod_r, tp);
        }

        let prev_r = self.btn_prev_model_page_rect();
        let next_r = self.btn_next_model_page_rect();
        for (btn_r, sym) in [(prev_r, "<"), (next_r, ">")] {
            if btn_r.contains(hx, hy) {
                fill_rrect(&rt, btn_r, 6.0, &surface_hover);
            } else {
                fill_rrect(&rt, btn_r, 6.0, &surface_bg);
            }
            stroke_rrect(&rt, btn_r, 6.0, 1.0, &border_subtle);
            if let Some(tp) = &text_primary {
                draw_text(&rt, &self.fmt_body_center, sym, btn_r, tp);
            }
        }

        // Fetched Models 6-Slot Grid (or Empty/Loading/Error State Box - R-27)
        if self.ai_config.fetched_models.is_empty() {
            let empty_box = RectF::new(24.0, 248.0, 512.0, 96.0);
            fill_rrect(&rt, empty_box, 6.0, &surface_bg);
            stroke_rrect(&rt, empty_box, 6.0, 1.0, &border_subtle);

            let msg = if self.fetching_models {
                "Sedang menghubungi endpoint /models provider untuk mengambil daftar model..."
            } else if self.fetch_is_error {
                &self.fetch_status_msg
            } else {
                "Daftar model belum dimuat. Masukkan API Key di atas lalu klik 'Tembak API & Muat Model' agar daftar model dari provider muncul di sini (atau ketik nama model secara manual)."
            };
            let msg_brush = if self.fetch_is_error {
                &text_error
            } else {
                &text_secondary
            };
            if let Some(b) = msg_brush {
                draw_text(
                    &rt,
                    &self.fmt_small_center,
                    msg,
                    RectF::new(
                        empty_box.x + 16.0,
                        empty_box.y + 12.0,
                        empty_box.width - 32.0,
                        empty_box.height - 24.0,
                    ),
                    b,
                );
            }
        } else {
            let start_idx = self.model_page * MODELS_PER_PAGE;
            for slot in 0..MODELS_PER_PAGE {
                let model_idx = start_idx + slot;
                if model_idx >= self.ai_config.fetched_models.len() {
                    break;
                }
                let model_name = &self.ai_config.fetched_models[model_idx];
                let card_r = self.fetched_model_card_rect(slot);
                let is_chosen = self.ai_config.selected_model == *model_name;

                if is_chosen {
                    fill_rrect(&rt, card_r, 6.0, &surface_selected);
                    stroke_rrect(&rt, card_r, 6.0, 1.5, &switch_on_green);
                } else if card_r.contains(hx, hy) {
                    fill_rrect(&rt, card_r, 6.0, &surface_hover);
                    stroke_rrect(&rt, card_r, 6.0, 1.0, &border_subtle);
                } else {
                    fill_rrect(&rt, card_r, 6.0, &surface_bg);
                    stroke_rrect(&rt, card_r, 6.0, 1.0, &border_subtle);
                }

                let col = if is_chosen {
                    &text_primary
                } else {
                    &text_secondary
                };
                if let Some(b) = col {
                    draw_text(
                        &rt,
                        &self.fmt_small_left,
                        model_name,
                        RectF::new(card_r.x + 10.0, card_r.y, card_r.width - 20.0, card_r.height),
                        b,
                    );
                }
            }
        }

        // Status line below model grid
        let status_brush = if self.fetch_is_error {
            &text_error
        } else {
            &text_secondary
        };
        if let Some(b) = status_brush {
            let total_pages = self.total_model_pages();
            let page_info = if !self.ai_config.fetched_models.is_empty() {
                format!(
                    "Hal {}/{} ({} model)  ·  {}",
                    self.model_page + 1,
                    total_pages,
                    self.ai_config.fetched_models.len(),
                    self.fetch_status_msg
                )
            } else {
                self.fetch_status_msg.clone()
            };
            draw_text(
                &rt,
                &self.fmt_small_left,
                &page_info,
                RectF::new(24.0, 349.0, 512.0, 20.0),
                b,
            );
        }

        draw_hline(&rt, 24.0, 536.0, 374.0, &divider_line);

        // ─────────────────────────────────────────────────────────────
        // 3. SHORTCUT PEMICU STEALTH
        // ─────────────────────────────────────────────────────────────
        if let Some(ts) = &text_secondary {
            draw_text(
                &rt,
                &self.fmt_section,
                "Shortcut Pemicu Stealth (Zero Focus Loss)",
                RectF::new(24.0, 382.0, 360.0, 18.0),
                ts,
            );
        }

        let active_bar = self.active_shortcut_bar_rect();
        fill_rrect(&rt, active_bar, 6.0, &surface_bg);
        if self.recording_custom {
            stroke_rrect(&rt, active_bar, 6.0, 1.5, &border_active);
            if let Some(tp) = &text_primary {
                draw_text(
                    &rt,
                    &self.fmt_small_left,
                    "Tekan kombinasi tombol keyboard atau klik mouse di sini...",
                    RectF::new(
                        active_bar.x + 12.0,
                        active_bar.y,
                        active_bar.width - 20.0,
                        active_bar.height,
                    ),
                    tp,
                );
            }
        } else {
            stroke_rrect(&rt, active_bar, 6.0, 1.0, &border_subtle);
            if let Some(ts) = &text_secondary {
                draw_text(
                    &rt,
                    &self.fmt_small_left,
                    "Aktif:",
                    RectF::new(active_bar.x + 12.0, active_bar.y, 44.0, active_bar.height),
                    ts,
                );
            }
            if let Some(tp) = &text_primary {
                draw_text(
                    &rt,
                    &self.fmt_body_left,
                    &self.active_shortcut.display_name(),
                    RectF::new(
                        active_bar.x + 56.0,
                        active_bar.y,
                        active_bar.width - 66.0,
                        active_bar.height,
                    ),
                    tp,
                );
            }
        }

        let rec_r = self.record_custom_btn_rect();
        if self.recording_custom {
            fill_rrect(&rt, rec_r, 6.0, &surface_selected);
            stroke_rrect(&rt, rec_r, 6.0, 1.5, &border_active);
        } else if rec_r.contains(hx, hy) {
            fill_rrect(&rt, rec_r, 6.0, &surface_hover);
            stroke_rrect(&rt, rec_r, 6.0, 1.0, &border_subtle);
        } else {
            fill_rrect(&rt, rec_r, 6.0, &surface_bg);
            stroke_rrect(&rt, rec_r, 6.0, 1.0, &border_subtle);
        }
        if let Some(tp) = &text_primary {
            let rec_txt = if self.recording_custom {
                "Batal Rekam"
            } else {
                "Rekam Kustom"
            };
            draw_text(&rt, &self.fmt_small_center, rec_txt, rec_r, tp);
        }

        let presets = ShortcutConfig::presets();
        let preset_labels = [
            "Ctrl + Win + X (Default)",
            "Ctrl + Win + Klik Kiri",
            "Tombol Samping Mouse (Stealth)",
            "Ctrl + Shift + Klik Kiri",
            "Alt + Win + Klik Kiri",
            "Ctrl + Win + Klik Tengah",
        ];

        for i in 0..6 {
            let rect = self.preset_card_rect(i);
            let is_selected = self.active_shortcut == presets[i] && !self.recording_custom;
            if is_selected {
                fill_rrect(&rt, rect, 6.0, &surface_selected);
                stroke_rrect(&rt, rect, 6.0, 1.5, &border_active);
            } else if rect.contains(hx, hy) {
                fill_rrect(&rt, rect, 6.0, &surface_hover);
                stroke_rrect(&rt, rect, 6.0, 1.0, &border_subtle);
            } else {
                fill_rrect(&rt, rect, 6.0, &surface_bg);
                stroke_rrect(&rt, rect, 6.0, 1.0, &border_subtle);
            }

            let label_color = if is_selected {
                &text_primary
            } else {
                &text_secondary
            };
            if let Some(b) = label_color {
                draw_text(
                    &rt,
                    &self.fmt_small_left,
                    preset_labels[i],
                    RectF::new(rect.x + 10.0, rect.y, rect.width - 20.0, rect.height),
                    b,
                );
            }
        }

        draw_hline(&rt, 24.0, 536.0, 550.0, &divider_line);

        // ─────────────────────────────────────────────────────────────
        // 4. MODE SELEKSI DEFAULT & OPSI SISTEM
        // ─────────────────────────────────────────────────────────────
        if let Some(ts) = &text_secondary {
            draw_text(
                &rt,
                &self.fmt_section,
                "Mode Seleksi Soal & Opsi Otomatis",
                RectF::new(24.0, 556.0, 340.0, 18.0),
                ts,
            );
        }

        let m_box = self.mode_box_rect();
        let box_active = self.selection_mode == SelectionMode::Box;
        if box_active {
            fill_rrect(&rt, m_box, 6.0, &surface_selected);
            stroke_rrect(&rt, m_box, 6.0, 1.5, &border_active);
        } else if m_box.contains(hx, hy) {
            fill_rrect(&rt, m_box, 6.0, &surface_hover);
            stroke_rrect(&rt, m_box, 6.0, 1.0, &border_subtle);
        } else {
            fill_rrect(&rt, m_box, 6.0, &surface_bg);
            stroke_rrect(&rt, m_box, 6.0, 1.0, &border_subtle);
        }
        if let Some(tp) = &text_primary {
            draw_text(
                &rt,
                &self.fmt_body_left,
                "Kotak Area (Box Snipping)",
                RectF::new(m_box.x + 12.0, m_box.y + 4.0, m_box.width - 24.0, 20.0),
                tp,
            );
        }
        if let Some(ts) = &text_secondary {
            draw_text(
                &rt,
                &self.fmt_small_left,
                "Default: Blok kotak gambar & teks soal",
                RectF::new(m_box.x + 12.0, m_box.y + 23.0, m_box.width - 24.0, 18.0),
                ts,
            );
        }

        let m_lin = self.mode_linear_rect();
        let lin_active = self.selection_mode == SelectionMode::Linear;
        if lin_active {
            fill_rrect(&rt, m_lin, 6.0, &surface_selected);
            stroke_rrect(&rt, m_lin, 6.0, 1.5, &border_active);
        } else if m_lin.contains(hx, hy) {
            fill_rrect(&rt, m_lin, 6.0, &surface_hover);
            stroke_rrect(&rt, m_lin, 6.0, 1.0, &border_subtle);
        } else {
            fill_rrect(&rt, m_lin, 6.0, &surface_bg);
            stroke_rrect(&rt, m_lin, 6.0, 1.0, &border_subtle);
        }
        if let Some(tp) = &text_primary {
            draw_text(
                &rt,
                &self.fmt_body_left,
                "Teks Linear (I-Beam)",
                RectF::new(m_lin.x + 12.0, m_lin.y + 4.0, m_lin.width - 24.0, 20.0),
                tp,
            );
        }
        if let Some(ts) = &text_secondary {
            draw_text(
                &rt,
                &self.fmt_small_left,
                "Seret kata per kata seperti CopyScreen",
                RectF::new(m_lin.x + 12.0, m_lin.y + 23.0, m_lin.width - 24.0, 18.0),
                ts,
            );
        }

        // Toggle 1: Auto-Copy Jawaban ke Clipboard
        let ac_r = self.autocopy_toggle_rect();
        if ac_r.contains(hx, hy) {
            fill_rrect(&rt, ac_r, 6.0, &surface_hover);
        } else {
            fill_rrect(&rt, ac_r, 6.0, &surface_bg);
        }
        stroke_rrect(&rt, ac_r, 6.0, 1.0, &border_subtle);
        if let Some(tp) = &text_primary {
            draw_text(
                &rt,
                &self.fmt_small_left,
                "Auto-Copy Jawaban AI",
                RectF::new(ac_r.x + 12.0, ac_r.y, ac_r.width - 68.0, ac_r.height),
                tp,
            );
        }
        draw_switch_pill(
            &rt,
            RectF::new(ac_r.right() - 52.0, ac_r.y + 10.0, 40.0, 22.0),
            self.ai_config.auto_copy_answer,
            &switch_on_green,
            &surface_bg,
            &border_subtle,
            &text_primary,
            &text_secondary,
        );

        // Toggle 2: Auto-Start Windows
        let as_r = self.autostart_toggle_rect();
        if as_r.contains(hx, hy) {
            fill_rrect(&rt, as_r, 6.0, &surface_hover);
        } else {
            fill_rrect(&rt, as_r, 6.0, &surface_bg);
        }
        stroke_rrect(&rt, as_r, 6.0, 1.0, &border_subtle);
        if let Some(tp) = &text_primary {
            draw_text(
                &rt,
                &self.fmt_small_left,
                "Auto-Start Saat PC Menyala",
                RectF::new(as_r.x + 12.0, as_r.y, as_r.width - 68.0, as_r.height),
                tp,
            );
        }
        draw_switch_pill(
            &rt,
            RectF::new(as_r.right() - 52.0, as_r.y + 10.0, 40.0, 22.0),
            self.autostart,
            &switch_on_green,
            &surface_bg,
            &border_subtle,
            &text_primary,
            &text_secondary,
        );

        draw_hline(&rt, 24.0, 536.0, 684.0, &divider_line);

        // ─────────────────────────────────────────────────────────────
        // 5. FOOTER ACTIONS
        // ─────────────────────────────────────────────────────────────
        let hide_r = self.btn_hide_tray_rect();
        if hide_r.contains(hx, hy) {
            fill_rrect(&rt, hide_r, 6.0, &surface_hover);
            stroke_rrect(&rt, hide_r, 6.0, 1.2, &border_active);
        } else {
            fill_rrect(&rt, hide_r, 6.0, &surface_bg);
            stroke_rrect(&rt, hide_r, 6.0, 1.0, &border_subtle);
        }
        if let Some(tp) = &text_primary {
            draw_text(
                &rt,
                &self.fmt_body_center,
                "Simpan & Sembunyikan ke System Tray",
                hide_r,
                tp,
            );
        }

        let exit_r = self.btn_exit_app_rect();
        fill_rrect(&rt, exit_r, 6.0, &danger_surface);
        if exit_r.contains(hx, hy) {
            stroke_rrect(&rt, exit_r, 6.0, 1.5, &danger_border);
        } else {
            stroke_rrect(&rt, exit_r, 6.0, 1.0, &danger_border);
        }
        if let Some(tp) = &text_primary {
            draw_text(&rt, &self.fmt_body_center, "Matikan Aplikasi", exit_r, tp);
        }

        let _ = &self.fmt_small_right;
        let _ = rt.EndDraw(None, None);
    }
}

unsafe fn draw_switch_pill(
    rt: &ID2D1HwndRenderTarget,
    sw_rect: RectF,
    on: bool,
    switch_on_green: &Option<ID2D1SolidColorBrush>,
    surface_bg: &Option<ID2D1SolidColorBrush>,
    border_subtle: &Option<ID2D1SolidColorBrush>,
    text_primary: &Option<ID2D1SolidColorBrush>,
    text_secondary: &Option<ID2D1SolidColorBrush>,
) {
    if on {
        fill_rrect(rt, sw_rect, 11.0, switch_on_green);
        let knob = RectF::new(sw_rect.right() - 19.0, sw_rect.y + 3.0, 16.0, 16.0);
        fill_rrect(rt, knob, 8.0, text_primary);
    } else {
        fill_rrect(rt, sw_rect, 11.0, surface_bg);
        stroke_rrect(rt, sw_rect, 11.0, 1.0, border_subtle);
        let knob = RectF::new(sw_rect.x + 3.0, sw_rect.y + 3.0, 16.0, 16.0);
        fill_rrect(rt, knob, 8.0, text_secondary);
    }
}

unsafe fn brush(
    rt: &ID2D1HwndRenderTarget,
    r: f32,
    g: f32,
    b: f32,
    a: f32,
) -> Option<ID2D1SolidColorBrush> {
    rt.CreateSolidColorBrush(&D2D1_COLOR_F { r, g, b, a }, None)
        .ok()
}

unsafe fn draw_aitask_header_logo(rt: &ID2D1HwndRenderTarget, bounds: RectF) {
    let x = bounds.x;
    let y = bounds.y;
    let s = bounds.width / 32.0;

    let bg = brush(rt, 0.067, 0.067, 0.067, 1.0);
    let border = brush(rt, 0.22, 0.22, 0.22, 1.0);
    let back_fill = brush(rt, 0.12, 0.16, 0.23, 1.0);
    let back_border = brush(rt, 0.22, 0.74, 0.97, 1.0);
    let front_fill = brush(rt, 0.95, 0.95, 0.95, 1.0);
    let emerald = brush(rt, 0.133, 0.773, 0.369, 1.0);
    let white = brush(rt, 1.0, 1.0, 1.0, 1.0);
    let dark_bar = brush(rt, 0.09, 0.09, 0.09, 1.0);
    let muted_bar = brush(rt, 0.45, 0.45, 0.45, 1.0);

    fill_rrect(rt, bounds, 7.5 * s, &bg);
    stroke_rrect(rt, bounds, 7.5 * s, 1.2, &border);

    let back = RectF::new(x + 5.5 * s, y + 5.2 * s, 15.2 * s, 15.2 * s);
    fill_rrect(rt, back, 2.5 * s, &back_fill);
    stroke_rrect(rt, back, 2.5 * s, 1.2, &back_border);

    let front = RectF::new(x + 10.2 * s, y + 10.0 * s, 16.0 * s, 16.0 * s);
    fill_rrect(rt, front, 2.6 * s, &front_fill);

    let grn = RectF::new(x + 12.2 * s, y + 12.4 * s, 12.0 * s, 4.0 * s);
    fill_rrect(rt, grn, 1.0 * s, &emerald);
    let wbar = RectF::new(x + 13.2 * s, y + 13.7 * s, 8.4 * s, 1.4 * s);
    fill_rrect(rt, wbar, 0.6 * s, &white);

    let bar1 = RectF::new(x + 12.2 * s, y + 18.0 * s, 11.0 * s, 1.6 * s);
    fill_rrect(rt, bar1, 0.6 * s, &dark_bar);
    let bar2 = RectF::new(x + 12.2 * s, y + 20.8 * s, 7.5 * s, 1.6 * s);
    fill_rrect(rt, bar2, 0.6 * s, &muted_bar);
}

unsafe fn draw_hline(
    rt: &ID2D1HwndRenderTarget,
    x1: f32,
    x2: f32,
    y: f32,
    brush: &Option<ID2D1SolidColorBrush>,
) {
    if let Some(b) = brush {
        rt.DrawLine(
            D2D_POINT_2F { x: x1, y },
            D2D_POINT_2F { x: x2, y },
            b,
            1.0,
            None,
        );
    }
}

unsafe fn fill_rrect(
    rt: &ID2D1HwndRenderTarget,
    rect: RectF,
    radius: f32,
    brush: &Option<ID2D1SolidColorBrush>,
) {
    if let Some(b) = brush {
        rt.FillRoundedRectangle(
            &D2D1_ROUNDED_RECT {
                rect: D2D_RECT_F {
                    left: rect.x,
                    top: rect.y,
                    right: rect.right(),
                    bottom: rect.bottom(),
                },
                radiusX: radius,
                radiusY: radius,
            },
            b,
        );
    }
}

unsafe fn stroke_rrect(
    rt: &ID2D1HwndRenderTarget,
    rect: RectF,
    radius: f32,
    width: f32,
    brush: &Option<ID2D1SolidColorBrush>,
) {
    if let Some(b) = brush {
        rt.DrawRoundedRectangle(
            &D2D1_ROUNDED_RECT {
                rect: D2D_RECT_F {
                    left: rect.x,
                    top: rect.y,
                    right: rect.right(),
                    bottom: rect.bottom(),
                },
                radiusX: radius,
                radiusY: radius,
            },
            b,
            width,
            None,
        );
    }
}

unsafe fn draw_text(
    rt: &ID2D1HwndRenderTarget,
    fmt: &IDWriteTextFormat,
    text: &str,
    rect: RectF,
    brush: &ID2D1SolidColorBrush,
) {
    let utf16: Vec<u16> = text.encode_utf16().collect();
    let d2d_rect = D2D_RECT_F {
        left: rect.x,
        top: rect.y,
        right: rect.right(),
        bottom: rect.bottom(),
    };
    rt.DrawText(
        &utf16,
        fmt,
        &d2d_rect,
        brush,
        D2D1_DRAW_TEXT_OPTIONS_CLIP,
        DWRITE_MEASURING_MODE_NATURAL,
    );
}

unsafe fn read_modifiers() -> (bool, bool, bool, bool) {
    let ctrl = (GetAsyncKeyState(VK_CONTROL.0 as i32) as u16 & 0x8000) != 0
        || (GetAsyncKeyState(VK_LCONTROL.0 as i32) as u16 & 0x8000) != 0
        || (GetAsyncKeyState(VK_RCONTROL.0 as i32) as u16 & 0x8000) != 0;
    let win = (GetAsyncKeyState(VK_LWIN.0 as i32) as u16 & 0x8000) != 0
        || (GetAsyncKeyState(VK_RWIN.0 as i32) as u16 & 0x8000) != 0;
    let alt = (GetAsyncKeyState(VK_MENU.0 as i32) as u16 & 0x8000) != 0
        || (GetAsyncKeyState(VK_LMENU.0 as i32) as u16 & 0x8000) != 0
        || (GetAsyncKeyState(VK_RMENU.0 as i32) as u16 & 0x8000) != 0;
    let shift = (GetAsyncKeyState(VK_SHIFT.0 as i32) as u16 & 0x8000) != 0
        || (GetAsyncKeyState(VK_LSHIFT.0 as i32) as u16 & 0x8000) != 0
        || (GetAsyncKeyState(VK_RSHIFT.0 as i32) as u16 & 0x8000) != 0;
    (ctrl, win, alt, shift)
}

pub fn run_dashboard_ui(initial_mode: SelectionMode) -> DashboardAction {
    ensure_default_autostart();

    let result = unsafe {
        (|| -> Option<DashboardAction> {
            let d2d_factory: ID2D1Factory =
                D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None).ok()?;
            let dwrite_factory: IDWriteFactory =
                DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED).ok()?;

            let fmt_title = dwrite_factory
                .CreateTextFormat(
                    w!("Segoe UI"),
                    None,
                    DWRITE_FONT_WEIGHT_BOLD,
                    DWRITE_FONT_STYLE_NORMAL,
                    DWRITE_FONT_STRETCH_NORMAL,
                    18.0,
                    w!("en-us"),
                )
                .ok()?;
            let _ = fmt_title.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER);

            let fmt_section = dwrite_factory
                .CreateTextFormat(
                    w!("Segoe UI"),
                    None,
                    DWRITE_FONT_WEIGHT_SEMI_BOLD,
                    DWRITE_FONT_STYLE_NORMAL,
                    DWRITE_FONT_STRETCH_NORMAL,
                    12.0,
                    w!("en-us"),
                )
                .ok()?;
            let _ = fmt_section.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER);

            let fmt_body_left = dwrite_factory
                .CreateTextFormat(
                    w!("Segoe UI"),
                    None,
                    DWRITE_FONT_WEIGHT_SEMI_BOLD,
                    DWRITE_FONT_STYLE_NORMAL,
                    DWRITE_FONT_STRETCH_NORMAL,
                    13.0,
                    w!("en-us"),
                )
                .ok()?;
            let _ = fmt_body_left.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_LEADING);
            let _ = fmt_body_left.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER);

            let fmt_body_center = dwrite_factory
                .CreateTextFormat(
                    w!("Segoe UI"),
                    None,
                    DWRITE_FONT_WEIGHT_SEMI_BOLD,
                    DWRITE_FONT_STYLE_NORMAL,
                    DWRITE_FONT_STRETCH_NORMAL,
                    13.0,
                    w!("en-us"),
                )
                .ok()?;
            let _ = fmt_body_center.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_CENTER);
            let _ = fmt_body_center.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER);

            let fmt_small_left = dwrite_factory
                .CreateTextFormat(
                    w!("Segoe UI"),
                    None,
                    DWRITE_FONT_WEIGHT_NORMAL,
                    DWRITE_FONT_STYLE_NORMAL,
                    DWRITE_FONT_STRETCH_NORMAL,
                    12.0,
                    w!("en-us"),
                )
                .ok()?;
            let _ = fmt_small_left.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_LEADING);
            let _ = fmt_small_left.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER);

            let fmt_small_right = dwrite_factory
                .CreateTextFormat(
                    w!("Segoe UI"),
                    None,
                    DWRITE_FONT_WEIGHT_NORMAL,
                    DWRITE_FONT_STYLE_NORMAL,
                    DWRITE_FONT_STRETCH_NORMAL,
                    12.0,
                    w!("en-us"),
                )
                .ok()?;
            let _ = fmt_small_right.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_TRAILING);
            let _ = fmt_small_right.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER);

            let fmt_small_center = dwrite_factory
                .CreateTextFormat(
                    w!("Segoe UI"),
                    None,
                    DWRITE_FONT_WEIGHT_SEMI_BOLD,
                    DWRITE_FONT_STYLE_NORMAL,
                    DWRITE_FONT_STRETCH_NORMAL,
                    12.0,
                    w!("en-us"),
                )
                .ok()?;
            let _ = fmt_small_center.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_CENTER);
            let _ = fmt_small_center.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER);

            let hinstance = GetModuleHandleW(None).ok()?;
            let class_name = w!("AITaskModernDashboardUIClass");
            let hicon: HICON = create_aitask_logo_icon().unwrap_or_default();

            let wc = WNDCLASSW {
                style: CS_HREDRAW | CS_VREDRAW,
                lpfnWndProc: Some(dashboard_wnd_proc),
                hInstance: hinstance.into(),
                lpszClassName: class_name,
                hIcon: hicon,
                hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
                ..Default::default()
            };
            let _ = RegisterClassW(&wc);

            let autostart_now = is_autostart_enabled();
            let ai_cfg = AiConfig::load();
            let init_msg = if ai_cfg.fetched_models.is_empty() {
                "Isi API Key lalu klik 'Tembak API & Muat Model' untuk memilih model.".to_string()
            } else {
                "Klik salah satu model di atas atau gunakan scroll mouse untuk ganti halaman."
                    .to_string()
            };

            let state = Box::new(DashboardState {
                hwnd: HWND::default(),
                d2d_factory,
                _dwrite_factory: dwrite_factory,
                fmt_title,
                fmt_section,
                fmt_body_left,
                fmt_body_center,
                fmt_small_left,
                fmt_small_right,
                fmt_small_center,
                rt: None,
                ui_scale: 1.0,
                ai_config: ai_cfg,
                active_input: ActiveTextInput::None,
                show_api_key: false,
                fetch_rx: None,
                fetching_models: false,
                fetch_status_msg: init_msg,
                fetch_is_error: false,
                model_page: 0,
                active_shortcut: get_active_shortcut(),
                recording_custom: false,
                selection_mode: initial_mode,
                autostart: autostart_now,
                hover: (0.0, 0.0),
                final_action: DashboardAction::MinimizeToTray(initial_mode),
            });
            let state_ptr = Box::into_raw(state);

            let screen_w = GetSystemMetrics(SM_CXSCREEN);
            let screen_h = GetSystemMetrics(SM_CYSCREEN);
            let win_w = WIN_WIDTH as i32 + 16;
            let win_h = WIN_HEIGHT as i32 + 39;
            let pos_x = ((screen_w - win_w) / 2).max(16);
            let pos_y = ((screen_h - win_h) / 2).max(16);

            let hwnd = CreateWindowExW(
                Default::default(),
                class_name,
                w!("AI-Task: Pengaturan AI, Model & Shortcut"),
                WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX | WS_VISIBLE,
                pos_x,
                pos_y,
                win_w,
                win_h,
                HWND::default(),
                None,
                hinstance,
                Some(state_ptr as *const _),
            )
            .ok()?;

            (*state_ptr).hwnd = hwnd;

            let dpi = GetDpiForWindow(hwnd);
            let raw_scale = if dpi > 0 {
                (dpi as f32 / 96.0).clamp(1.0, 2.5)
            } else {
                1.0
            };
            let max_scale = ((screen_h - 60) as f32 / WIN_HEIGHT as f32).clamp(1.0, 2.5);
            let ui_scale = raw_scale.min(max_scale);
            (*state_ptr).ui_scale = ui_scale;

            if (ui_scale - 1.0).abs() > 0.01 {
                let mut rect = windows::Win32::Foundation::RECT {
                    left: 0,
                    top: 0,
                    right: (WIN_WIDTH as f32 * ui_scale).round() as i32,
                    bottom: (WIN_HEIGHT as f32 * ui_scale).round() as i32,
                };
                let style = WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX | WS_VISIBLE;
                let _ = AdjustWindowRectExForDpi(&mut rect, style, false, WINDOW_EX_STYLE(0), dpi);
                let scaled_w = rect.right - rect.left;
                let scaled_h = rect.bottom - rect.top;
                let new_x = ((screen_w - scaled_w) / 2).max(10);
                let new_y = ((screen_h - scaled_h) / 2).max(10);
                let _ = SetWindowPos(
                    hwnd,
                    HWND_TOP,
                    new_x,
                    new_y,
                    scaled_w,
                    scaled_h,
                    SWP_NOZORDER,
                );
            }

            let dark_mode: i32 = 1;
            let _ = DwmSetWindowAttribute(
                hwnd,
                DWMWA_USE_IMMERSIVE_DARK_MODE,
                &dark_mode as *const _ as *const c_void,
                4,
            );
            let round_pref = DWMWCP_ROUND;
            let _ = DwmSetWindowAttribute(
                hwnd,
                DWMWA_WINDOW_CORNER_PREFERENCE,
                &round_pref as *const _ as *const c_void,
                4,
            );

            if !hicon.is_invalid() {
                let _ = SendMessageW(
                    hwnd,
                    WM_SETICON,
                    WPARAM(ICON_SMALL as usize),
                    LPARAM(hicon.0 as isize),
                );
                let _ = SendMessageW(
                    hwnd,
                    WM_SETICON,
                    WPARAM(ICON_BIG as usize),
                    LPARAM(hicon.0 as isize),
                );
            }

            let _ = ShowWindow(hwnd, SW_SHOW);
            let _ = SetForegroundWindow(hwnd);

            let mut msg = MSG::default();
            while GetMessageW(&mut msg, HWND::default(), 0, 0).as_bool() {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
                if !IsWindow(hwnd).as_bool() {
                    break;
                }
            }

            if !hicon.is_invalid() {
                let _ = DestroyIcon(hicon);
            }

            let final_state = Box::from_raw(state_ptr);
            let _ = final_state.ai_config.save();
            Some(final_state.final_action)
        })()
    };

    trim_process_working_set();
    result.unwrap_or(DashboardAction::MinimizeToTray(initial_mode))
}

unsafe extern "system" fn dashboard_wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if msg == WM_NCCREATE {
        let cs = lparam.0 as *const CREATESTRUCTW;
        if !cs.is_null() {
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, (*cs).lpCreateParams as isize);
        }
        return DefWindowProcW(hwnd, msg, wparam, lparam);
    }

    let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut DashboardState;
    if ptr.is_null() {
        return DefWindowProcW(hwnd, msg, wparam, lparam);
    }
    let state = &mut *ptr;

    match msg {
        WM_ERASEBKGND => LRESULT(1),

        WM_TIMER => {
            if wparam.0 == FETCH_POLL_TIMER_ID {
                state.poll_fetch_models();
            }
            LRESULT(0)
        }

        WM_SIZE => {
            if let Some(rt) = &state.rt {
                let w = (lparam.0 & 0xFFFF) as u32;
                let h = ((lparam.0 >> 16) & 0xFFFF) as u32;
                if w > 0 && h > 0 {
                    let _ = rt.Resize(&D2D_SIZE_U {
                        width: w,
                        height: h,
                    });
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

        WM_MOUSEMOVE => {
            let x = ((lparam.0 & 0xFFFF) as i16 as f32) / state.ui_scale;
            let y = (((lparam.0 >> 16) & 0xFFFF) as i16 as f32) / state.ui_scale;
            state.hover = (x, y);
            let _ = InvalidateRect(hwnd, None, false);
            LRESULT(0)
        }

        WM_MOUSEWHEEL => {
            let total_pages = state.total_model_pages();
            if total_pages > 1 {
                let delta = (wparam.0 >> 16) as i16;
                if delta < 0 && state.model_page + 1 < total_pages {
                    state.model_page += 1;
                    let _ = InvalidateRect(hwnd, None, false);
                } else if delta > 0 && state.model_page > 0 {
                    state.model_page -= 1;
                    let _ = InvalidateRect(hwnd, None, false);
                }
            }
            LRESULT(0)
        }

        WM_SETCURSOR => {
            let (hx, hy) = state.hover;
            let cursor_id = if state.is_text_input_at(hx, hy) {
                IDC_IBEAM
            } else if state.is_interactive_at(hx, hy) {
                IDC_HAND
            } else {
                IDC_ARROW
            };
            if let Ok(cur) = LoadCursorW(None, cursor_id) {
                SetCursor(cur);
            }
            LRESULT(1)
        }

        WM_LBUTTONDOWN => {
            let x = ((lparam.0 & 0xFFFF) as i16 as f32) / state.ui_scale;
            let y = (((lparam.0 >> 16) & 0xFFFF) as i16 as f32) / state.ui_scale;

            // Custom shortcut recording click on active_shortcut_bar
            let active_bar = state.active_shortcut_bar_rect();
            if state.recording_custom && active_bar.contains(x, y) {
                let (ctrl, win, alt, shift) = read_modifiers();
                if ctrl || win || alt || shift {
                    let new_sc = ShortcutConfig {
                        ctrl,
                        win,
                        alt,
                        shift,
                        trigger: TriggerInput::MouseLeftClick,
                    };
                    let _ = save_shortcut(new_sc);
                    state.active_shortcut = new_sc;
                    state.recording_custom = false;
                    let _ = InvalidateRect(hwnd, None, false);
                    return LRESULT(0);
                }
            }

            // Text input focus
            if state.input_api_key_rect().contains(x, y) {
                state.active_input = ActiveTextInput::ApiKey;
                state.recording_custom = false;
                let _ = InvalidateRect(hwnd, None, false);
                return LRESULT(0);
            }
            if state.ai_config.provider == AiProvider::Custom
                && state.input_base_url_rect().contains(x, y)
            {
                state.active_input = ActiveTextInput::BaseUrl;
                state.recording_custom = false;
                let _ = InvalidateRect(hwnd, None, false);
                return LRESULT(0);
            }
            if state.input_model_manual_rect().contains(x, y) {
                state.active_input = ActiveTextInput::ModelManual;
                state.recording_custom = false;
                let _ = InvalidateRect(hwnd, None, false);
                return LRESULT(0);
            }
            state.active_input = ActiveTextInput::None;

            // Header CTA
            if state.btn_scan_now_rect().contains(x, y) {
                let _ = state.ai_config.save();
                state.final_action = DashboardAction::TriggerScanNow(state.selection_mode);
                let _ = DestroyWindow(hwnd);
                return LRESULT(0);
            }

            // Provider tabs
            let providers = AiProvider::all();
            for (i, prov) in providers.iter().copied().enumerate() {
                if state.provider_tab_rect(i).contains(x, y) {
                    state.switch_provider(prov);
                    let _ = InvalidateRect(hwnd, None, false);
                    return LRESULT(0);
                }
            }

            // Paste API Key
            if state.btn_paste_key_rect().contains(x, y) {
                if let Ok(clip) = read_text_from_clipboard() {
                    let trimmed = clip.trim();
                    if !trimmed.is_empty() {
                        state.ai_config.api_key = trimmed.to_string();
                        let _ = state.ai_config.save();
                        state.fetch_is_error = false;
                        state.fetch_status_msg =
                            "API Key ditempel dari Clipboard. Klik 'Muat Model' untuk mengambil model."
                                .to_string();
                    }
                }
                let _ = InvalidateRect(hwnd, None, false);
                return LRESULT(0);
            }

            // Show/Hide API Key
            if state.btn_toggle_show_key_rect().contains(x, y) {
                state.show_api_key = !state.show_api_key;
                let _ = InvalidateRect(hwnd, None, false);
                return LRESULT(0);
            }

            // Paste Custom Base URL
            if state.ai_config.provider == AiProvider::Custom
                && state.btn_paste_url_rect().contains(x, y)
            {
                if let Ok(clip) = read_text_from_clipboard() {
                    let trimmed = clip.trim();
                    if !trimmed.is_empty() {
                        state.ai_config.custom_base_url = trimmed.to_string();
                        let _ = state.ai_config.save();
                    }
                }
                let _ = InvalidateRect(hwnd, None, false);
                return LRESULT(0);
            }

            // Fetch Models Button
            if state.btn_fetch_models_rect().contains(x, y) {
                state.trigger_fetch_models();
                return LRESULT(0);
            }

            // Paste Manual Model
            if state.btn_paste_model_rect().contains(x, y) {
                if let Ok(clip) = read_text_from_clipboard() {
                    let trimmed = clip.trim();
                    if !trimmed.is_empty() {
                        state.ai_config.selected_model = trimmed.to_string();
                        let _ = state.ai_config.save();
                    }
                }
                let _ = InvalidateRect(hwnd, None, false);
                return LRESULT(0);
            }

            // Model pagination
            if state.btn_prev_model_page_rect().contains(x, y) {
                if state.model_page > 0 {
                    state.model_page -= 1;
                    let _ = InvalidateRect(hwnd, None, false);
                }
                return LRESULT(0);
            }
            if state.btn_next_model_page_rect().contains(x, y) {
                let total_pages = state.total_model_pages();
                if state.model_page + 1 < total_pages {
                    state.model_page += 1;
                    let _ = InvalidateRect(hwnd, None, false);
                }
                return LRESULT(0);
            }

            // Fetched Model Grid Cards
            let start_idx = state.model_page * MODELS_PER_PAGE;
            for slot in 0..MODELS_PER_PAGE {
                let model_idx = start_idx + slot;
                if model_idx < state.ai_config.fetched_models.len()
                    && state.fetched_model_card_rect(slot).contains(x, y)
                {
                    let chosen = state.ai_config.fetched_models[model_idx].clone();
                    state.ai_config.selected_model = chosen.clone();
                    let _ = state.ai_config.save();
                    state.fetch_is_error = false;
                    state.fetch_status_msg = format!("Model '{chosen}' aktif dan siap digunakan.");
                    let _ = InvalidateRect(hwnd, None, false);
                    return LRESULT(0);
                }
            }

            // Shortcut Recorder button
            if state.record_custom_btn_rect().contains(x, y) {
                state.recording_custom = !state.recording_custom;
                let _ = InvalidateRect(hwnd, None, false);
                return LRESULT(0);
            }

            // 6 Shortcut Presets
            let presets = ShortcutConfig::presets();
            for i in 0..6 {
                if state.preset_card_rect(i).contains(x, y) {
                    let chosen = presets[i];
                    let _ = save_shortcut(chosen);
                    state.active_shortcut = chosen;
                    state.recording_custom = false;
                    let _ = InvalidateRect(hwnd, None, false);
                    return LRESULT(0);
                }
            }

            // Selection Mode Box vs Linear
            if state.mode_box_rect().contains(x, y) {
                state.selection_mode = SelectionMode::Box;
                state.final_action = DashboardAction::MinimizeToTray(state.selection_mode);
                let _ = InvalidateRect(hwnd, None, false);
                return LRESULT(0);
            }
            if state.mode_linear_rect().contains(x, y) {
                state.selection_mode = SelectionMode::Linear;
                state.final_action = DashboardAction::MinimizeToTray(state.selection_mode);
                let _ = InvalidateRect(hwnd, None, false);
                return LRESULT(0);
            }

            // Auto-Copy Answer Toggle
            if state.autocopy_toggle_rect().contains(x, y) {
                state.ai_config.auto_copy_answer = !state.ai_config.auto_copy_answer;
                let _ = state.ai_config.save();
                let _ = InvalidateRect(hwnd, None, false);
                return LRESULT(0);
            }

            // Auto-Start Toggle
            if state.autostart_toggle_rect().contains(x, y) {
                let next = !state.autostart;
                if set_autostart_enabled(next).is_ok() {
                    state.autostart = next;
                }
                let _ = InvalidateRect(hwnd, None, false);
                return LRESULT(0);
            }

            // Footer Hide & Exit
            if state.btn_hide_tray_rect().contains(x, y) {
                let _ = state.ai_config.save();
                state.final_action = DashboardAction::MinimizeToTray(state.selection_mode);
                let _ = DestroyWindow(hwnd);
                return LRESULT(0);
            }
            if state.btn_exit_app_rect().contains(x, y) {
                let _ = state.ai_config.save();
                state.final_action = DashboardAction::ExitApplication;
                let _ = DestroyWindow(hwnd);
                return LRESULT(0);
            }

            let _ = InvalidateRect(hwnd, None, false);
            LRESULT(0)
        }

        WM_MBUTTONDOWN => {
            if state.recording_custom {
                let (ctrl, win, alt, shift) = read_modifiers();
                let new_sc = ShortcutConfig {
                    ctrl,
                    win,
                    alt,
                    shift,
                    trigger: TriggerInput::MouseMiddleClick,
                };
                let _ = save_shortcut(new_sc);
                state.active_shortcut = new_sc;
                state.recording_custom = false;
                let _ = InvalidateRect(hwnd, None, false);
            }
            LRESULT(0)
        }

        WM_RBUTTONDOWN => {
            if state.recording_custom {
                let (ctrl, win, alt, shift) = read_modifiers();
                if ctrl || win || alt || shift {
                    let new_sc = ShortcutConfig {
                        ctrl,
                        win,
                        alt,
                        shift,
                        trigger: TriggerInput::MouseRightClick,
                    };
                    let _ = save_shortcut(new_sc);
                    state.active_shortcut = new_sc;
                    state.recording_custom = false;
                    let _ = InvalidateRect(hwnd, None, false);
                }
            }
            LRESULT(0)
        }

        WM_XBUTTONDOWN => {
            if state.recording_custom {
                let (ctrl, win, alt, shift) = read_modifiers();
                let new_sc = ShortcutConfig {
                    ctrl,
                    win,
                    alt,
                    shift,
                    trigger: TriggerInput::MouseXButton1,
                };
                let _ = save_shortcut(new_sc);
                state.active_shortcut = new_sc;
                state.recording_custom = false;
                let _ = InvalidateRect(hwnd, None, false);
            }
            LRESULT(0)
        }

        WM_CHAR => {
            if state.active_input != ActiveTextInput::None {
                let ch_code = wparam.0 as u32;
                let ctrl_down = (GetAsyncKeyState(VK_CONTROL.0 as i32) as u16 & 0x8000) != 0;
                if ctrl_down {
                    return LRESULT(0);
                }

                let target_str = match state.active_input {
                    ActiveTextInput::ApiKey => &mut state.ai_config.api_key,
                    ActiveTextInput::BaseUrl => &mut state.ai_config.custom_base_url,
                    ActiveTextInput::ModelManual => &mut state.ai_config.selected_model,
                    ActiveTextInput::None => return LRESULT(0),
                };

                if ch_code == 0x08 {
                    // Backspace
                    target_str.pop();
                    let _ = state.ai_config.save();
                    let _ = InvalidateRect(hwnd, None, false);
                } else if ch_code >= 0x20 {
                    if let Some(ch) = char::from_u32(ch_code) {
                        if !ch.is_control() {
                            target_str.push(ch);
                            let _ = state.ai_config.save();
                            let _ = InvalidateRect(hwnd, None, false);
                        }
                    }
                }
                return LRESULT(0);
            }
            LRESULT(0)
        }

        WM_KEYDOWN | WM_SYSKEYDOWN => {
            let vk = wparam.0 as u16;
            if vk == VK_ESCAPE.0 {
                if state.recording_custom {
                    state.recording_custom = false;
                    let _ = InvalidateRect(hwnd, None, false);
                } else if state.active_input != ActiveTextInput::None {
                    state.active_input = ActiveTextInput::None;
                    let _ = InvalidateRect(hwnd, None, false);
                } else {
                    let _ = state.ai_config.save();
                    state.final_action = DashboardAction::MinimizeToTray(state.selection_mode);
                    let _ = DestroyWindow(hwnd);
                }
                return LRESULT(0);
            }

            if state.recording_custom {
                let is_mod = matches!(vk, 0x10..=0x12 | 0x5B | 0x5C | 0xA0..=0xA5);
                if !is_mod {
                    let (ctrl, win, alt, shift) = read_modifiers();
                    if ctrl || win || alt || shift {
                        let new_sc = ShortcutConfig {
                            ctrl,
                            win,
                            alt,
                            shift,
                            trigger: TriggerInput::Key(vk),
                        };
                        let _ = save_shortcut(new_sc);
                        state.active_shortcut = new_sc;
                        state.recording_custom = false;
                        let _ = InvalidateRect(hwnd, None, false);
                    }
                }
                return LRESULT(0);
            }

            if state.active_input != ActiveTextInput::None {
                let ctrl_down = (GetAsyncKeyState(VK_CONTROL.0 as i32) as u16 & 0x8000) != 0;
                // Ctrl+V paste into active input
                if ctrl_down && vk == b'V' as u16 {
                    if let Ok(clip) = read_text_from_clipboard() {
                        let clean = clip.trim();
                        if !clean.is_empty() {
                            match state.active_input {
                                ActiveTextInput::ApiKey => {
                                    state.ai_config.api_key = clean.to_string()
                                }
                                ActiveTextInput::BaseUrl => {
                                    state.ai_config.custom_base_url = clean.to_string()
                                }
                                ActiveTextInput::ModelManual => {
                                    state.ai_config.selected_model = clean.to_string()
                                }
                                ActiveTextInput::None => {}
                            }
                            let _ = state.ai_config.save();
                            let _ = InvalidateRect(hwnd, None, false);
                        }
                    }
                    return LRESULT(0);
                }
                // Delete key or Ctrl+Backspace clears the active field
                if vk == 0x2E {
                    match state.active_input {
                        ActiveTextInput::ApiKey => state.ai_config.api_key.clear(),
                        ActiveTextInput::BaseUrl => state.ai_config.custom_base_url.clear(),
                        ActiveTextInput::ModelManual => state.ai_config.selected_model.clear(),
                        ActiveTextInput::None => {}
                    }
                    let _ = state.ai_config.save();
                    let _ = InvalidateRect(hwnd, None, false);
                    return LRESULT(0);
                }
                // Enter inside API Key or Base URL triggers model fetch
                if vk == 0x0D {
                    let was_api = matches!(
                        state.active_input,
                        ActiveTextInput::ApiKey | ActiveTextInput::BaseUrl
                    );
                    state.active_input = ActiveTextInput::None;
                    if was_api {
                        state.trigger_fetch_models();
                    } else {
                        let _ = InvalidateRect(hwnd, None, false);
                    }
                    return LRESULT(0);
                }
            }

            LRESULT(0)
        }

        WM_CLOSE => {
            let _ = state.ai_config.save();
            state.final_action = DashboardAction::MinimizeToTray(state.selection_mode);
            let _ = DestroyWindow(hwnd);
            LRESULT(0)
        }

        WM_DESTROY => {
            let _ = KillTimer(hwnd, FETCH_POLL_TIMER_ID);
            state.rt = None;
            LRESULT(0)
        }

        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}
