use std::collections::BTreeMap;
use std::ffi::c_void;
use std::mem::size_of;
use std::ptr;
use windows::core::{w, PCWSTR};
use windows::Win32::Networking::WinHttp::{
    WinHttpAddRequestHeaders, WinHttpCloseHandle, WinHttpConnect, WinHttpOpen, WinHttpOpenRequest,
    WinHttpQueryDataAvailable, WinHttpQueryHeaders, WinHttpReadData, WinHttpReceiveResponse,
    WinHttpSendRequest, WinHttpSetTimeouts, WINHTTP_ACCESS_TYPE, WINHTTP_ADDREQ_FLAG_ADD,
    WINHTTP_ADDREQ_FLAG_REPLACE, WINHTTP_FLAG_SECURE, WINHTTP_OPEN_REQUEST_FLAGS,
    WINHTTP_QUERY_FLAG_NUMBER, WINHTTP_QUERY_STATUS_CODE,
};
use windows::Win32::System::Registry::{
    RegCloseKey, RegCreateKeyExW, RegOpenKeyExW, RegQueryValueExW, RegSetValueExW, HKEY,
    HKEY_CURRENT_USER, KEY_READ, KEY_WRITE, REG_DWORD, REG_OPTION_NON_VOLATILE, REG_SZ,
};

const SETTINGS_REG_SUBKEY: PCWSTR = w!("Software\\AITask");

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AiProvider {
    Gemini,
    OpenAi,
    Groq,
    OpenRouter,
    Custom,
}

impl AiProvider {
    pub fn all() -> [Self; 5] {
        [
            Self::Gemini,
            Self::OpenAi,
            Self::Groq,
            Self::OpenRouter,
            Self::Custom,
        ]
    }

    pub fn id_str(self) -> &'static str {
        match self {
            Self::Gemini => "gemini",
            Self::OpenAi => "openai",
            Self::Groq => "groq",
            Self::OpenRouter => "openrouter",
            Self::Custom => "custom",
        }
    }

    pub fn from_id_str(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().as_str() {
            "openai" => Self::OpenAi,
            "groq" => Self::Groq,
            "openrouter" => Self::OpenRouter,
            "custom" => Self::Custom,
            _ => Self::Gemini,
        }
    }

    pub fn display_name(self) -> &'static str {
        match self {
            Self::Gemini => "Gemini",
            Self::OpenAi => "OpenAI",
            Self::Groq => "Groq",
            Self::OpenRouter => "OpenRouter",
            Self::Custom => "Custom URL",
        }
    }

    pub fn default_base_url(self) -> &'static str {
        match self {
            Self::Gemini => "https://generativelanguage.googleapis.com/v1beta",
            Self::OpenAi => "https://api.openai.com/v1",
            Self::Groq => "https://api.groq.com/openai/v1",
            Self::OpenRouter => "https://openrouter.ai/api/v1",
            Self::Custom => "https://api.openai.com/v1",
        }
    }
}

#[derive(Debug, Clone)]
pub struct AiConfig {
    pub provider: AiProvider,
    pub api_key: String,
    pub custom_base_url: String,
    pub selected_model: String,
    pub fetched_models: Vec<String>,
    pub auto_copy_answer: bool,
}

impl Default for AiConfig {
    fn default() -> Self {
        Self {
            provider: AiProvider::Gemini,
            api_key: String::new(),
            custom_base_url: "https://api.openai.com/v1".to_string(),
            selected_model: String::new(),
            fetched_models: Vec::new(),
            auto_copy_answer: true,
        }
    }
}

impl AiConfig {
    pub fn effective_base_url(&self) -> String {
        let raw = if self.provider == AiProvider::Custom {
            self.custom_base_url.trim()
        } else {
            self.provider.default_base_url()
        };
        raw.trim_end_matches('/').to_string()
    }

    pub fn is_ready(&self) -> bool {
        let needs_key = self.provider != AiProvider::Custom
            || !self.effective_base_url().contains("localhost");
        (!needs_key || !self.api_key.trim().is_empty()) && !self.selected_model.trim().is_empty()
    }

    pub fn load() -> Self {
        let provider_str = read_reg_string("ActiveProvider").unwrap_or_else(|| "gemini".to_string());
        let provider = AiProvider::from_id_str(&provider_str);
        Self::load_for_provider(provider)
    }

    pub fn load_for_provider(provider: AiProvider) -> Self {
        let pid = provider.id_str();
        let api_key = read_reg_string(&format!("ApiKey_{pid}")).unwrap_or_default();
        let selected_model = read_reg_string(&format!("Model_{pid}")).unwrap_or_default();
        let cache_raw = read_reg_string(&format!("ModelsCache_{pid}")).unwrap_or_default();
        let fetched_models: Vec<String> = cache_raw
            .lines()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        let custom_base_url = read_reg_string("CustomBaseUrl")
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| "https://api.openai.com/v1".to_string());
        let auto_copy_answer = read_reg_dword("AutoCopyAnswer").unwrap_or(1) != 0;

        Self {
            provider,
            api_key,
            custom_base_url,
            selected_model,
            fetched_models,
            auto_copy_answer,
        }
    }

    pub fn save(&self) -> Result<(), String> {
        let pid = self.provider.id_str();
        write_reg_string("ActiveProvider", pid)?;
        write_reg_string(&format!("ApiKey_{pid}"), self.api_key.trim())?;
        write_reg_string(&format!("Model_{pid}"), self.selected_model.trim())?;
        let joined_models = self.fetched_models.join("\n");
        write_reg_string(&format!("ModelsCache_{pid}"), &joined_models)?;
        write_reg_string("CustomBaseUrl", self.custom_base_url.trim())?;
        write_reg_dword(
            "AutoCopyAnswer",
            if self.auto_copy_answer { 1 } else { 0 },
        )?;
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AiAnswer {
    pub is_multiple_choice: bool,
    pub headline: String,
    pub body: String,
    pub copy_payload: String,
    pub model_used: String,
}

/// Hits the configured provider's `/models` API endpoint with the user's API key
/// and returns the list of model names available to choose from.
pub fn fetch_provider_models(config: &AiConfig) -> Result<Vec<String>, String> {
    let base = config.effective_base_url();
    if base.is_empty() {
        return Err("Base URL API kosong.".to_string());
    }

    let key = config.api_key.trim();
    if key.is_empty() && !base.contains("localhost") && !base.contains("127.0.0.1") {
        return Err("Masukkan API Key terlebih dahulu sebelum memuat daftar model.".to_string());
    }

    match config.provider {
        AiProvider::Gemini => {
            let url = format!("{base}/models?pageSize=100&key={key}");
            let (status, body) = winhttp_request("GET", &url, &[], None)?;
            if !(200..300).contains(&status) {
                let detail = extract_api_error_message(&body)
                    .unwrap_or_else(|| format!("HTTP {status}"));
                return Err(format!("Gagal memuat model Gemini: {detail}"));
            }
            let models = parse_gemini_models_json(&body)?;
            if models.is_empty() {
                return Err("API merespons sukses, tetapi tidak ada model generateContent ditemukan.".to_string());
            }
            Ok(models)
        }
        AiProvider::OpenAi | AiProvider::Groq | AiProvider::OpenRouter | AiProvider::Custom => {
            let url = format!("{base}/models");
            let mut headers = Vec::new();
            if !key.is_empty() {
                headers.push(("Authorization".to_string(), format!("Bearer {key}")));
            }
            let (status, body) = winhttp_request("GET", &url, &headers, None)?;
            if !(200..300).contains(&status) {
                let detail = extract_api_error_message(&body)
                    .unwrap_or_else(|| format!("HTTP {status}"));
                return Err(format!(
                    "Gagal memuat model {}: {detail}",
                    config.provider.display_name()
                ));
            }
            let models = parse_openai_compatible_models_json(&body, config.provider)?;
            if models.is_empty() {
                return Err("API tidak mengembalikan daftar model.".to_string());
            }
            Ok(models)
        }
    }
}

const SYSTEM_INSTRUCTION: &str = "Kamu adalah asisten ahli penjawab soal latihan akademik (Pilihan Ganda maupun Esai/Uraian/Hitungan).
Analisis gambar tangkapan soal dan teks OCR yang diberikan secara teliti, lalu ikuti aturan format output berikut Tanpa Terkecuali:

1. JIKA SOAL ADALAH PILIHAN GANDA (terdapat opsi A/B/C/D/E atau pilihan jawaban):
   - Baris pertama WAJIB diawali dengan `TIPE: PILIHAN_GANDA`
   - Baris kedua WAJIB diawali dengan `JAWABAN: ` diikuti huruf dan teks opsi yang benar secara langsung dan tegas (contoh: `JAWABAN: B. Mitokondria`).
   - Baris ketiga (opsional, maksimal 1 kalimat padat): `INTI: ` diikuti alasan super singkat mengapa opsi itu benar.

2. JIKA SOAL BUKAN PILIHAN GANDA (soal esai, uraian, isian singkat, hitungan, atau perintah 'Jelaskan...', 'Uraikan...', 'Sebutkan...'):
   - Baris pertama WAJIB diawali dengan `TIPE: URAIAN`
   - Baris kedua WAJIB diawali dengan `JAWABAN: ` diikuti ringkasan jawaban inti (1 baris).
   - Baris ketiga dan seterusnya diawali dengan `PENJELASAN:` lalu berikan penjelasan/langkah jawaban yang jelas, terstruktur, natural, dan langsung siap dipakai menjawab soal.
   - Jangan gunakan karakter em-dash (—) dan jangan gunakan basa-basi pembuka.";

/// Solves a captured question using Hybrid Vision (cropped PNG Base64) + extracted OCR text.
pub fn solve_question_hybrid(
    config: &AiConfig,
    cropped_png_base64: Option<&str>,
    ocr_text: &str,
) -> Result<AiAnswer, String> {
    let model = config.selected_model.trim();
    if model.is_empty() {
        return Err(
            "Model AI belum dipilih. Buka Pengaturan AI-Task di System Tray, masukkan API Key, lalu klik 'Muat Model'."
                .to_string(),
        );
    }

    let base = config.effective_base_url();
    let key = config.api_key.trim();
    if key.is_empty() && !base.contains("localhost") && !base.contains("127.0.0.1") {
        return Err(
            "API Key belum diisi. Klik ikon AI-Task di System Tray untuk mengisi API Key."
                .to_string(),
        );
    }

    let user_prompt = if ocr_text.trim().is_empty() {
        "Baca soal pada gambar tangkapan layar ini dengan teliti dan berikan jawabannya sesuai format.".to_string()
    } else {
        format!(
            "Berikut hasil ekstraksi teks OCR dari area soal yang diblok (gunakan bersama gambar untuk membaca rumus/tabel/opsi secara akurat):\n\n{}\n\nBerikan jawaban yang paling tepat sesuai format.",
            ocr_text.trim()
        )
    };

    let raw_reply = match config.provider {
        AiProvider::Gemini => call_gemini_generate_content(
            &base,
            key,
            model,
            cropped_png_base64,
            &user_prompt,
        )?,
        AiProvider::OpenAi | AiProvider::Groq | AiProvider::OpenRouter | AiProvider::Custom => {
            match call_openai_chat_completions(
                &base,
                key,
                model,
                cropped_png_base64,
                &user_prompt,
            ) {
                Ok(ans) => ans,
                Err(err) => {
                    // Automatic fallback: if user selected a text-only model that rejects `image_url`,
                    // retry immediately with OCR text alone!
                    if cropped_png_base64.is_some() && !ocr_text.trim().is_empty() {
                        call_openai_chat_completions(&base, key, model, None, &user_prompt)
                            .map_err(|_| err)?
                    } else {
                        return Err(err);
                    }
                }
            }
        }
    };

    Ok(parse_structured_ai_answer(&raw_reply, model))
}

fn call_gemini_generate_content(
    base_url: &str,
    api_key: &str,
    model: &str,
    png_base64: Option<&str>,
    user_prompt: &str,
) -> Result<String, String> {
    let clean_model = model.trim_start_matches("models/");
    let url = format!("{base_url}/models/{clean_model}:generateContent?key={api_key}");

    let combined_text = format!("{SYSTEM_INSTRUCTION}\n\n{user_prompt}");
    let payload = if let Some(b64) = png_base64.filter(|s| !s.is_empty()) {
        format!(
            r#"{{"contents":[{{"parts":[{{"inline_data":{{"mime_type":"image/png","data":"{}"}}}},{{"text":"{}"}}]}}],"generationConfig":{{"temperature":0.15}}}}"#,
            json_escape(b64),
            json_escape(&combined_text)
        )
    } else {
        format!(
            r#"{{"contents":[{{"parts":[{{"text":"{}"}}]}}],"generationConfig":{{"temperature":0.15}}}}"#,
            json_escape(&combined_text)
        )
    };

    let headers = [("Content-Type".to_string(), "application/json".to_string())];
    let (status, body) = winhttp_request("POST", &url, &headers, Some(payload.as_bytes()))?;

    if !(200..300).contains(&status) {
        let msg = extract_api_error_message(&body).unwrap_or_else(|| format!("HTTP {status}"));
        return Err(format!("Gemini API Error: {msg}"));
    }

    extract_gemini_reply_text(&body)
}

fn call_openai_chat_completions(
    base_url: &str,
    api_key: &str,
    model: &str,
    png_base64: Option<&str>,
    user_prompt: &str,
) -> Result<String, String> {
    let url = format!("{base_url}/chat/completions");

    let payload = if let Some(b64) = png_base64.filter(|s| !s.is_empty()) {
        format!(
            r#"{{"model":"{}","temperature":0.15,"messages":[{{"role":"system","content":"{}"}},{{"role":"user","content":[{{"type":"text","text":"{}"}},{{"type":"image_url","image_url":{{"url":"data:image/png;base64,{}"}}}}]}}]}}"#,
            json_escape(model),
            json_escape(SYSTEM_INSTRUCTION),
            json_escape(user_prompt),
            json_escape(b64)
        )
    } else {
        format!(
            r#"{{"model":"{}","temperature":0.15,"messages":[{{"role":"system","content":"{}"}},{{"role":"user","content":"{}"}}]}}"#,
            json_escape(model),
            json_escape(SYSTEM_INSTRUCTION),
            json_escape(user_prompt)
        )
    };

    let mut headers = vec![("Content-Type".to_string(), "application/json".to_string())];
    if !api_key.is_empty() {
        headers.push(("Authorization".to_string(), format!("Bearer {api_key}")));
    }

    let (status, body) = winhttp_request("POST", &url, &headers, Some(payload.as_bytes()))?;
    if !(200..300).contains(&status) {
        let msg = extract_api_error_message(&body).unwrap_or_else(|| format!("HTTP {status}"));
        return Err(format!("API Error ({model}): {msg}"));
    }

    extract_openai_reply_text(&body)
}

pub fn parse_structured_ai_answer(raw: &str, model_used: &str) -> AiAnswer {
    let cleaned = raw
        .replace('—', " - ")
        .replace("**", "")
        .trim()
        .to_string();

    let mut is_mc = false;
    let mut is_essay = false;
    let mut jawaban_line = String::new();
    let mut explanation_lines: Vec<String> = Vec::new();
    let mut in_penjelasan = false;

    for line in cleaned.lines() {
        let t = line.trim();
        if t.is_empty() {
            if in_penjelasan && !explanation_lines.is_empty() {
                explanation_lines.push(String::new());
            }
            continue;
        }

        let upper = t.to_ascii_uppercase();
        if upper.starts_with("TIPE:") {
            if upper.contains("PILIHAN_GANDA") || upper.contains("PILIHAN GANDA") {
                is_mc = true;
            } else if upper.contains("URAIAN") || upper.contains("ESAI") {
                is_essay = true;
            }
            continue;
        }

        if upper.starts_with("JAWABAN:") && jawaban_line.is_empty() {
            jawaban_line = t["JAWABAN:".len()..].trim().to_string();
            continue;
        }

        if upper.starts_with("INTI:") {
            let inti = t["INTI:".len()..].trim();
            if !inti.is_empty() {
                explanation_lines.push(inti.to_string());
            }
            continue;
        }

        if upper.starts_with("PENJELASAN:") {
            in_penjelasan = true;
            let rest = t["PENJELASAN:".len()..].trim();
            if !rest.is_empty() {
                explanation_lines.push(rest.to_string());
            }
            continue;
        }

        explanation_lines.push(t.to_string());
    }

    // Heuristic fallback if the model did not include the explicit `TIPE:` line
    if !is_mc && !is_essay {
        let candidate = if !jawaban_line.is_empty() {
            jawaban_line.as_str()
        } else {
            cleaned.lines().next().unwrap_or("").trim()
        };
        let chars: Vec<char> = candidate.chars().collect();
        let looks_like_option = chars.len() >= 2
            && matches!(chars[0].to_ascii_uppercase(), 'A'..='E')
            && matches!(chars[1], '.' | ')' | ':' | ' ');
        if looks_like_option && explanation_lines.join(" ").len() < 240 {
            is_mc = true;
        }
    }

    if is_mc {
        let headline = if !jawaban_line.is_empty() {
            jawaban_line.clone()
        } else {
            cleaned
                .lines()
                .find(|l| !l.trim().is_empty())
                .unwrap_or(&cleaned)
                .trim()
                .to_string()
        };
        let body = explanation_lines.join("\n").trim().to_string();
        AiAnswer {
            is_multiple_choice: true,
            copy_payload: headline.clone(),
            headline,
            body,
            model_used: model_used.to_string(),
        }
    } else {
        let headline = if !jawaban_line.is_empty() {
            jawaban_line.clone()
        } else {
            "Jawaban & Penjelasan".to_string()
        };
        let body = if !explanation_lines.is_empty() {
            explanation_lines.join("\n").trim().to_string()
        } else {
            cleaned.clone()
        };
        let copy_payload = if !jawaban_line.is_empty() && !body.is_empty() && body != jawaban_line {
            format!("{jawaban_line}\n\n{body}")
        } else if !body.is_empty() {
            body.clone()
        } else {
            headline.clone()
        };

        AiAnswer {
            is_multiple_choice: false,
            headline,
            body,
            copy_payload,
            model_used: model_used.to_string(),
        }
    }
}

pub fn parse_gemini_models_json(body: &str) -> Result<Vec<String>, String> {
    let val = JsonValue::parse(body)?;
    let Some(models_arr) = val.get_field("models").and_then(|v| v.as_array()) else {
        return Err("Format JSON Gemini tidak memiliki array 'models'.".to_string());
    };

    let mut models = Vec::new();
    for item in models_arr {
        let Some(raw_name) = item.get_field("name").and_then(|v| v.as_str()) else {
            continue;
        };
        let short_name = raw_name.trim_start_matches("models/");
        if short_name.is_empty() {
            continue;
        }

        // Check if supportedGenerationMethods includes "generateContent"
        let supports_generate = match item
            .get_field("supportedGenerationMethods")
            .and_then(|v| v.as_array())
        {
            Some(methods) => methods
                .iter()
                .any(|m| m.as_str() == Some("generateContent")),
            None => short_name.starts_with("gemini-"),
        };

        let lower = short_name.to_ascii_lowercase();
        if !supports_generate
            || lower.contains("embedding")
            || lower.contains("aqa")
            || lower.contains("tts")
            || lower.contains("imagen")
            || lower.contains("veo")
        {
            continue;
        }

        models.push(short_name.to_string());
    }

    models.sort_by(|a, b| model_priority_key(a).cmp(&model_priority_key(b)));
    models.dedup();
    Ok(models)
}

pub fn parse_openai_compatible_models_json(
    body: &str,
    provider: AiProvider,
) -> Result<Vec<String>, String> {
    let val = JsonValue::parse(body)?;

    // Standard OpenAI/Groq/OpenRouter uses `{ "data": [ { "id": "..." } ] }`
    // Some custom servers (e.g. Ollama) may return `{ "models": [ { "name": "..." } ] }`
    let arr = val
        .get_field("data")
        .and_then(|v| v.as_array())
        .or_else(|| val.get_field("models").and_then(|v| v.as_array()))
        .or_else(|| val.as_array())
        .ok_or_else(|| "Format JSON tidak memiliki array 'data' atau 'models'.".to_string())?;

    let mut models = Vec::new();
    for item in arr {
        let id = item
            .get_field("id")
            .and_then(|v| v.as_str())
            .or_else(|| item.get_field("name").and_then(|v| v.as_str()));
        let Some(raw_id) = id else { continue };
        let clean = raw_id.trim();
        if clean.is_empty() {
            continue;
        }

        let lower = clean.to_ascii_lowercase();
        if provider == AiProvider::OpenAi || provider == AiProvider::Groq {
            if lower.contains("whisper")
                || lower.contains("tts")
                || lower.contains("dall-e")
                || lower.contains("embedding")
                || lower.contains("moderation")
                || lower.starts_with("babbage")
                || lower.starts_with("davinci")
                || lower.contains("audio")
                || lower.contains("realtime")
                || lower.contains("guard")
            {
                continue;
            }
        }

        models.push(clean.to_string());
    }

    models.sort_by(|a, b| model_priority_key(a).cmp(&model_priority_key(b)));
    models.dedup();
    Ok(models)
}

fn model_priority_key(name: &str) -> (u8, String) {
    let l = name.to_ascii_lowercase();
    let tier = if l.contains("gemini-2.5-flash")
        || l.contains("gpt-4o")
        || l.contains("llama-4-scout")
        || l.contains("claude-3.5-sonnet")
        || l.contains("claude-3-7-sonnet")
    {
        0
    } else if l.contains("gemini-2.5-pro")
        || l.contains("gemini-2.0-flash")
        || l.contains("gpt-4.1")
        || l.contains("o3")
        || l.contains("o4-mini")
        || l.contains("llama-3.2-90b-vision")
        || l.contains("llama-3.3-70b")
    {
        1
    } else if l.contains("gemini-1.5") || l.contains("vision") || l.contains("vl") {
        2
    } else {
        3
    };
    (tier, l)
}

fn extract_gemini_reply_text(body: &str) -> Result<String, String> {
    let val = JsonValue::parse(body)?;
    if let Some(candidates) = val.get_field("candidates").and_then(|v| v.as_array()) {
        for cand in candidates {
            if let Some(parts) = cand
                .get_field("content")
                .and_then(|c| c.get_field("parts"))
                .and_then(|p| p.as_array())
            {
                let mut combined = String::new();
                for part in parts {
                    if let Some(t) = part.get_field("text").and_then(|v| v.as_str()) {
                        if !combined.is_empty() {
                            combined.push('\n');
                        }
                        combined.push_str(t);
                    }
                }
                if !combined.trim().is_empty() {
                    return Ok(combined);
                }
            }
        }
    }
    Err("Respons Gemini tidak berisi teks jawaban.".to_string())
}

fn extract_openai_reply_text(body: &str) -> Result<String, String> {
    let val = JsonValue::parse(body)?;
    if let Some(choices) = val.get_field("choices").and_then(|v| v.as_array()) {
        for choice in choices {
            if let Some(content) = choice
                .get_field("message")
                .and_then(|m| m.get_field("content"))
            {
                if let Some(s) = content.as_str() {
                    if !s.trim().is_empty() {
                        return Ok(s.to_string());
                    }
                }
                if let Some(arr) = content.as_array() {
                    let mut combined = String::new();
                    for item in arr {
                        if let Some(t) = item.get_field("text").and_then(|v| v.as_str()) {
                            combined.push_str(t);
                        }
                    }
                    if !combined.trim().is_empty() {
                        return Ok(combined);
                    }
                }
            }
        }
    }
    Err("Respons API tidak berisi pesan jawaban.".to_string())
}

fn extract_api_error_message(body: &str) -> Option<String> {
    let val = JsonValue::parse(body).ok()?;
    if let Some(err) = val.get_field("error") {
        if let Some(msg) = err.get_field("message").and_then(|v| v.as_str()) {
            return Some(msg.to_string());
        }
        if let Some(msg) = err.as_str() {
            return Some(msg.to_string());
        }
    }
    val.get_field("message")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
}

pub fn json_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 16);
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                out.push_str(&format!("\\u{:04x}", c as u32));
            }
            c => out.push(c),
        }
    }
    out
}

#[derive(Debug, Clone, PartialEq)]
pub struct ParsedUrl {
    pub is_https: bool,
    pub host: String,
    pub port: u16,
    pub path_and_query: String,
}

pub fn parse_http_url(url: &str) -> Result<ParsedUrl, String> {
    let trimmed = url.trim();
    let (is_https, rest) = if let Some(r) = trimmed.strip_prefix("https://") {
        (true, r)
    } else if let Some(r) = trimmed.strip_prefix("http://") {
        (false, r)
    } else {
        return Err(format!("URL harus diawali https:// atau http:// ({trimmed})"));
    };

    let (host_port, path_and_query) = match rest.find('/') {
        Some(idx) => (&rest[..idx], rest[idx..].to_string()),
        None => (rest, "/".to_string()),
    };

    if host_port.is_empty() {
        return Err("Host URL kosong.".to_string());
    }

    let (host, port) = match host_port.split_once(':') {
        Some((h, p_str)) => {
            let p: u16 = p_str
                .parse()
                .map_err(|_| format!("Port URL tidak valid: {p_str}"))?;
            (h.to_string(), p)
        }
        None => (host_port.to_string(), if is_https { 443 } else { 80 }),
    };

    Ok(ParsedUrl {
        is_https,
        host,
        port,
        path_and_query,
    })
}

fn to_wide_null(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Performs an HTTP/HTTPS request using Windows native WinHTTP (zero external dependencies, uses OS TLS 1.2/1.3).
pub fn winhttp_request(
    method: &str,
    url: &str,
    headers: &[(String, String)],
    body: Option<&[u8]>,
) -> Result<(u16, String), String> {
    let parsed = parse_http_url(url)?;

    unsafe {
        // WINHTTP_ACCESS_TYPE_DEFAULT_PROXY = 0
        let hsession = WinHttpOpen(
            w!("AI-Task/0.1 (Windows NT; Native WinHTTP)"),
            WINHTTP_ACCESS_TYPE(0),
            PCWSTR::null(),
            PCWSTR::null(),
            0,
        );
        if hsession.is_null() {
            return Err("WinHttpOpen gagal menginisialisasi sesi jaringan.".to_string());
        }

        let _ = WinHttpSetTimeouts(hsession, 8000, 10000, 20000, 45000);

        let host_w = to_wide_null(&parsed.host);
        let hconnect = WinHttpConnect(
            hsession,
            PCWSTR(host_w.as_ptr()),
            parsed.port,
            0,
        );
        if hconnect.is_null() {
            let _ = WinHttpCloseHandle(hsession);
            return Err(format!("Gagal terhubung ke host {}", parsed.host));
        }

        let method_w = to_wide_null(method);
        let path_w = to_wide_null(&parsed.path_and_query);
        let flags = if parsed.is_https {
            WINHTTP_FLAG_SECURE
        } else {
            WINHTTP_OPEN_REQUEST_FLAGS(0)
        };

        let hrequest = WinHttpOpenRequest(
            hconnect,
            PCWSTR(method_w.as_ptr()),
            PCWSTR(path_w.as_ptr()),
            PCWSTR::null(),
            PCWSTR::null(),
            ptr::null(),
            flags,
        );
        if hrequest.is_null() {
            let _ = WinHttpCloseHandle(hconnect);
            let _ = WinHttpCloseHandle(hsession);
            return Err("WinHttpOpenRequest gagal membuat request.".to_string());
        }

        for (k, v) in headers {
            let line = format!("{k}: {v}\r\n");
            let line_w: Vec<u16> = line.encode_utf16().collect();
            let _ = WinHttpAddRequestHeaders(
                hrequest,
                &line_w,
                WINHTTP_ADDREQ_FLAG_ADD | WINHTTP_ADDREQ_FLAG_REPLACE,
            );
        }

        let (body_ptr, body_len) = match body {
            Some(b) if !b.is_empty() => (Some(b.as_ptr() as *const c_void), b.len() as u32),
            _ => (None, 0u32),
        };

        if let Err(e) = WinHttpSendRequest(hrequest, None, body_ptr, body_len, body_len, 0) {
            let _ = WinHttpCloseHandle(hrequest);
            let _ = WinHttpCloseHandle(hconnect);
            let _ = WinHttpCloseHandle(hsession);
            return Err(format!("Gagal mengirim request ke {}: {e}", parsed.host));
        }

        if let Err(e) = WinHttpReceiveResponse(hrequest, ptr::null_mut()) {
            let _ = WinHttpCloseHandle(hrequest);
            let _ = WinHttpCloseHandle(hconnect);
            let _ = WinHttpCloseHandle(hsession);
            return Err(format!("Gagal menerima respons dari {}: {e}", parsed.host));
        }

        let mut status_code: u32 = 0;
        let mut status_len = size_of::<u32>() as u32;
        let mut index: u32 = 0;
        let _ = WinHttpQueryHeaders(
            hrequest,
            WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER,
            PCWSTR::null(),
            Some(&mut status_code as *mut u32 as *mut c_void),
            &mut status_len,
            &mut index,
        );

        let mut response_bytes = Vec::new();
        loop {
            let mut avail: u32 = 0;
            if WinHttpQueryDataAvailable(hrequest, &mut avail).is_err() || avail == 0 {
                break;
            }
            let mut chunk = vec![0u8; avail as usize];
            let mut read: u32 = 0;
            if WinHttpReadData(
                hrequest,
                chunk.as_mut_ptr() as *mut c_void,
                avail,
                &mut read,
            )
            .is_err()
                || read == 0
            {
                break;
            }
            response_bytes.extend_from_slice(&chunk[..read as usize]);
        }

        let _ = WinHttpCloseHandle(hrequest);
        let _ = WinHttpCloseHandle(hconnect);
        let _ = WinHttpCloseHandle(hsession);

        let response_str = String::from_utf8_lossy(&response_bytes).to_string();
        Ok((status_code as u16, response_str))
    }
}

// ─────────────────────────────────────────────────────────────
// Lightweight, Dependency-Free Recursive-Descent JSON Parser
// ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
pub enum JsonValue {
    Null,
    Bool(bool),
    Number(f64),
    Str(String),
    Array(Vec<JsonValue>),
    Object(BTreeMap<String, JsonValue>),
}

impl JsonValue {
    pub fn parse(input: &str) -> Result<Self, String> {
        let chars: Vec<char> = input.chars().collect();
        let mut pos = 0;
        skip_ws(&chars, &mut pos);
        let val = parse_value(&chars, &mut pos)?;
        Ok(val)
    }

    pub fn get_field(&self, key: &str) -> Option<&JsonValue> {
        match self {
            JsonValue::Object(map) => map.get(key),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            JsonValue::Str(s) => Some(s.as_str()),
            _ => None,
        }
    }

    pub fn as_array(&self) -> Option<&[JsonValue]> {
        match self {
            JsonValue::Array(arr) => Some(arr.as_slice()),
            _ => None,
        }
    }
}

fn skip_ws(chars: &[char], pos: &mut usize) {
    while *pos < chars.len() && chars[*pos].is_whitespace() {
        *pos += 1;
    }
}

fn parse_value(chars: &[char], pos: &mut usize) -> Result<JsonValue, String> {
    skip_ws(chars, pos);
    if *pos >= chars.len() {
        return Err("JSON berakhir secara tidak terduga.".to_string());
    }
    match chars[*pos] {
        '{' => parse_object(chars, pos),
        '[' => parse_array(chars, pos),
        '"' => parse_string(chars, pos).map(JsonValue::Str),
        't' | 'f' => parse_bool(chars, pos),
        'n' => parse_null(chars, pos),
        c if c == '-' || c.is_ascii_digit() => parse_number(chars, pos),
        other => Err(format!("Karakter JSON tidak valid: '{other}' pada posisi {}", *pos)),
    }
}

fn parse_object(chars: &[char], pos: &mut usize) -> Result<JsonValue, String> {
    *pos += 1; // '{'
    skip_ws(chars, pos);
    let mut map = BTreeMap::new();
    if *pos < chars.len() && chars[*pos] == '}' {
        *pos += 1;
        return Ok(JsonValue::Object(map));
    }

    loop {
        skip_ws(chars, pos);
        if *pos >= chars.len() || chars[*pos] != '"' {
            return Err("Key objek JSON harus berupa string.".to_string());
        }
        let key = parse_string(chars, pos)?;
        skip_ws(chars, pos);
        if *pos >= chars.len() || chars[*pos] != ':' {
            return Err("Diharapkan ':' setelah key objek JSON.".to_string());
        }
        *pos += 1;
        let val = parse_value(chars, pos)?;
        map.insert(key, val);

        skip_ws(chars, pos);
        if *pos >= chars.len() {
            return Err("Objek JSON tidak ditutup dengan '}'.".to_string());
        }
        if chars[*pos] == '}' {
            *pos += 1;
            break;
        } else if chars[*pos] == ',' {
            *pos += 1;
        } else {
            return Err(format!("Karakter tidak terduga di dalam objek JSON: '{}'", chars[*pos]));
        }
    }
    Ok(JsonValue::Object(map))
}

fn parse_array(chars: &[char], pos: &mut usize) -> Result<JsonValue, String> {
    *pos += 1; // '['
    skip_ws(chars, pos);
    let mut arr = Vec::new();
    if *pos < chars.len() && chars[*pos] == ']' {
        *pos += 1;
        return Ok(JsonValue::Array(arr));
    }

    loop {
        let val = parse_value(chars, pos)?;
        arr.push(val);
        skip_ws(chars, pos);
        if *pos >= chars.len() {
            return Err("Array JSON tidak ditutup dengan ']'.".to_string());
        }
        if chars[*pos] == ']' {
            *pos += 1;
            break;
        } else if chars[*pos] == ',' {
            *pos += 1;
        } else {
            return Err(format!("Karakter tidak terduga di dalam array JSON: '{}'", chars[*pos]));
        }
    }
    Ok(JsonValue::Array(arr))
}

fn parse_string(chars: &[char], pos: &mut usize) -> Result<String, String> {
    *pos += 1; // '"'
    let mut out = String::new();
    while *pos < chars.len() {
        let ch = chars[*pos];
        *pos += 1;
        if ch == '"' {
            return Ok(out);
        }
        if ch == '\\' {
            if *pos >= chars.len() {
                return Err("Escape string JSON tidak lengkap.".to_string());
            }
            let esc = chars[*pos];
            *pos += 1;
            match esc {
                '"' => out.push('"'),
                '\\' => out.push('\\'),
                '/' => out.push('/'),
                'b' => out.push('\u{0008}'),
                'f' => out.push('\u{000C}'),
                'n' => out.push('\n'),
                'r' => out.push('\r'),
                't' => out.push('\t'),
                'u' => {
                    if *pos + 4 > chars.len() {
                        return Err("Escape unicode \\uXXXX tidak lengkap.".to_string());
                    }
                    let hex: String = chars[*pos..*pos + 4].iter().collect();
                    *pos += 4;
                    let code = u16::from_str_radix(&hex, 16)
                        .map_err(|_| format!("Kode hex unicode tidak valid: {hex}"))?;
                    // Check UTF-16 surrogate pair
                    if (0xD800..=0xDBFF).contains(&code)
                        && *pos + 6 <= chars.len()
                        && chars[*pos] == '\\'
                        && chars[*pos + 1] == 'u'
                    {
                        let hex2: String = chars[*pos + 2..*pos + 6].iter().collect();
                        if let Ok(low) = u16::from_str_radix(&hex2, 16) {
                            if (0xDC00..=0xDFFF).contains(&low) {
                                *pos += 6;
                                let combined = 0x10000
                                    + (((code as u32 - 0xD800) << 10) | (low as u32 - 0xDC00));
                                if let Some(c) = char::from_u32(combined) {
                                    out.push(c);
                                    continue;
                                }
                            }
                        }
                    }
                    if let Some(c) = char::from_u32(code as u32) {
                        out.push(c);
                    }
                }
                other => out.push(other),
            }
        } else {
            out.push(ch);
        }
    }
    Err("String JSON tidak ditutup tanda kutip.".to_string())
}

fn parse_bool(chars: &[char], pos: &mut usize) -> Result<JsonValue, String> {
    if chars[*pos..].starts_with(&['t', 'r', 'u', 'e']) {
        *pos += 4;
        Ok(JsonValue::Bool(true))
    } else if chars[*pos..].starts_with(&['f', 'a', 'l', 's', 'e']) {
        *pos += 5;
        Ok(JsonValue::Bool(false))
    } else {
        Err("Token boolean JSON tidak valid.".to_string())
    }
}

fn parse_null(chars: &[char], pos: &mut usize) -> Result<JsonValue, String> {
    if chars[*pos..].starts_with(&['n', 'u', 'l', 'l']) {
        *pos += 4;
        Ok(JsonValue::Null)
    } else {
        Err("Token null JSON tidak valid.".to_string())
    }
}

fn parse_number(chars: &[char], pos: &mut usize) -> Result<JsonValue, String> {
    let start = *pos;
    while *pos < chars.len()
        && matches!(
            chars[*pos],
            '0'..='9' | '-' | '+' | '.' | 'e' | 'E'
        )
    {
        *pos += 1;
    }
    let s: String = chars[start..*pos].iter().collect();
    let n: f64 = s
        .parse()
        .map_err(|_| format!("Angka JSON tidak valid: {s}"))?;
    Ok(JsonValue::Number(n))
}

// ─────────────────────────────────────────────────────────────
// Registry Helpers
// ─────────────────────────────────────────────────────────────

fn read_reg_string(name: &str) -> Option<String> {
    unsafe {
        let mut hkey = HKEY::default();
        if RegOpenKeyExW(HKEY_CURRENT_USER, SETTINGS_REG_SUBKEY, 0, KEY_READ, &mut hkey).is_err() {
            return None;
        }
        let name_w = to_wide_null(name);
        let mut byte_len: u32 = 0;
        if RegQueryValueExW(
            hkey,
            PCWSTR(name_w.as_ptr()),
            None,
            None,
            None,
            Some(&mut byte_len),
        )
        .is_err()
            || byte_len < 2
        {
            let _ = RegCloseKey(hkey);
            return None;
        }

        let u16_cap = (byte_len as usize + 1) / 2;
        let mut buf = vec![0u16; u16_cap];
        let ok = RegQueryValueExW(
            hkey,
            PCWSTR(name_w.as_ptr()),
            None,
            None,
            Some(buf.as_mut_ptr() as *mut u8),
            Some(&mut byte_len),
        )
        .is_ok();
        let _ = RegCloseKey(hkey);

        if ok {
            let s = String::from_utf16_lossy(&buf);
            Some(s.trim_end_matches('\0').to_string())
        } else {
            None
        }
    }
}

fn write_reg_string(name: &str, value: &str) -> Result<(), String> {
    unsafe {
        let mut hkey = HKEY::default();
        RegCreateKeyExW(
            HKEY_CURRENT_USER,
            SETTINGS_REG_SUBKEY,
            0,
            None,
            REG_OPTION_NON_VOLATILE,
            KEY_WRITE,
            None,
            &mut hkey,
            None,
        )
        .ok()
        .map_err(|e| format!("Gagal membuka registry AI-Task: {e}"))?;

        let name_w = to_wide_null(name);
        let val_w = to_wide_null(value);
        let bytes =
            std::slice::from_raw_parts(val_w.as_ptr() as *const u8, val_w.len() * size_of::<u16>());
        let res = RegSetValueExW(hkey, PCWSTR(name_w.as_ptr()), 0, REG_SZ, Some(bytes))
            .ok()
            .map_err(|e| format!("Gagal menyimpan {name} ke registry: {e}"));
        let _ = RegCloseKey(hkey);
        res
    }
}

fn read_reg_dword(name: &str) -> Option<u32> {
    unsafe {
        let mut hkey = HKEY::default();
        if RegOpenKeyExW(HKEY_CURRENT_USER, SETTINGS_REG_SUBKEY, 0, KEY_READ, &mut hkey).is_err() {
            return None;
        }
        let name_w = to_wide_null(name);
        let mut val: u32 = 0;
        let mut len = 4u32;
        let ok = RegQueryValueExW(
            hkey,
            PCWSTR(name_w.as_ptr()),
            None,
            None,
            Some(&mut val as *mut u32 as *mut u8),
            Some(&mut len),
        )
        .is_ok();
        let _ = RegCloseKey(hkey);
        if ok {
            Some(val)
        } else {
            None
        }
    }
}

fn write_reg_dword(name: &str, val: u32) -> Result<(), String> {
    unsafe {
        let mut hkey = HKEY::default();
        RegCreateKeyExW(
            HKEY_CURRENT_USER,
            SETTINGS_REG_SUBKEY,
            0,
            None,
            REG_OPTION_NON_VOLATILE,
            KEY_WRITE,
            None,
            &mut hkey,
            None,
        )
        .ok()
        .map_err(|e| format!("Gagal membuka registry AI-Task: {e}"))?;

        let name_w = to_wide_null(name);
        let bytes = std::slice::from_raw_parts(&val as *const u32 as *const u8, 4);
        let res = RegSetValueExW(hkey, PCWSTR(name_w.as_ptr()), 0, REG_DWORD, Some(bytes))
            .ok()
            .map_err(|e| format!("Gagal menyimpan {name} ke registry: {e}"));
        let _ = RegCloseKey(hkey);
        res
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_gemini_models_list_and_prioritizes_flash_and_pro() {
        let sample = r#"{
            "models": [
                {
                    "name": "models/text-embedding-004",
                    "supportedGenerationMethods": ["embedContent"]
                },
                {
                    "name": "models/gemini-1.5-flash",
                    "supportedGenerationMethods": ["generateContent", "countTokens"]
                },
                {
                    "name": "models/gemini-2.5-flash",
                    "supportedGenerationMethods": ["generateContent", "countTokens"]
                },
                {
                    "name": "models/gemini-2.5-pro",
                    "supportedGenerationMethods": ["generateContent"]
                }
            ]
        }"#;

        let models = parse_gemini_models_json(sample).expect("Should parse Gemini models");
        assert_eq!(
            models,
            vec!["gemini-2.5-flash", "gemini-2.5-pro", "gemini-1.5-flash"]
        );
    }

    #[test]
    fn parses_openai_compatible_models_and_filters_non_chat() {
        let sample = r#"{
            "object": "list",
            "data": [
                {"id": "whisper-large-v3", "object": "model"},
                {"id": "llama-3.3-70b-versatile", "object": "model"},
                {"id": "meta-llama/llama-4-scout-17b-16e-instruct", "object": "model"},
                {"id": "text-embedding-3-small", "object": "model"}
            ]
        }"#;

        let models = parse_openai_compatible_models_json(sample, AiProvider::Groq)
            .expect("Should parse Groq/OpenAI models");
        assert_eq!(
            models,
            vec![
                "meta-llama/llama-4-scout-17b-16e-instruct",
                "llama-3.3-70b-versatile"
            ]
        );
    }

    #[test]
    fn formats_multiple_choice_and_essay_answers_distinctly() {
        let mc_raw = "TIPE: PILIHAN_GANDA\nJAWABAN: C. Hukum II Newton (F = m . a)\nINTI: Percepatan berbanding lurus dengan resultan gaya.";
        let mc = parse_structured_ai_answer(mc_raw, "gemini-2.5-flash");
        assert!(mc.is_multiple_choice);
        assert_eq!(mc.headline, "C. Hukum II Newton (F = m . a)");
        assert_eq!(mc.copy_payload, "C. Hukum II Newton (F = m . a)");
        assert_eq!(mc.body, "Percepatan berbanding lurus dengan resultan gaya.");

        let essay_raw = "TIPE: URAIAN\nJAWABAN: Fotosintesis mengubah energi cahaya menjadi glukosa dan oksigen.\nPENJELASAN:\nReaksi terang terjadi di tilakoid menghasilkan ATP dan NADPH.\nSiklus Calvin terjadi di stroma mengikat CO2 menjadi glukosa.";
        let essay = parse_structured_ai_answer(essay_raw, "gpt-4o");
        assert!(!essay.is_multiple_choice);
        assert_eq!(
            essay.headline,
            "Fotosintesis mengubah energi cahaya menjadi glukosa dan oksigen."
        );
        assert!(essay.body.contains("Reaksi terang terjadi di tilakoid"));
        assert!(essay.copy_payload.contains("Siklus Calvin"));
    }

    #[test]
    fn parses_https_and_http_urls() {
        let u1 = parse_http_url("https://generativelanguage.googleapis.com/v1beta/models?key=123")
            .unwrap();
        assert!(u1.is_https);
        assert_eq!(u1.host, "generativelanguage.googleapis.com");
        assert_eq!(u1.port, 443);
        assert_eq!(u1.path_and_query, "/v1beta/models?key=123");

        let u2 = parse_http_url("http://localhost:11434/v1/models").unwrap();
        assert!(!u2.is_https);
        assert_eq!(u2.host, "localhost");
        assert_eq!(u2.port, 11434);
        assert_eq!(u2.path_and_query, "/v1/models");
    }
}
