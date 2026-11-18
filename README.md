# AI-Task

**Asisten Penjawab Soal Latihan Layar Ultra-Ringan (100% Pure Rust + Native Win32/Direct2D/OneOCR/WinHTTP) untuk Windows 10 & 11**

---

## Tentang AI-Task

**AI-Task** adalah utilitas *desktop* berbasis **100% Pure Rust + Native Windows API (`Win32`, `Direct2D`, `DirectWrite`, `OneOCR`, & `WinHTTP`)** yang memungkinkan kamu memblok area soal apa pun di layar dengan menekan **`Ctrl + Win + X`**, lalu mendapatkan jawaban AI secara langsung di kartu melayang (*Floating Stealth Answer HUD*) tanpa pernah memindahkan fokus jendela (`WS_EX_NOACTIVATE`).

Dirancang tanpa Electron, tanpa WebView, dan tanpa *runtime* tambahan sehingga ukuran file `.exe` hanya sekitar **`~450 KB`**.

---

## Fitur Utama

1. **Shortcut Stealth (`Ctrl + Win + X` Default, Bisa Dikustomisasi)**
   - Menahan `WM_KEYDOWN` sekaligus `WM_KEYUP` di level OS (`WH_KEYBOARD_LL`) agar tombol pemicu tidak pernah bocor ke aplikasi atau browser di bawahnya.
   - Mendukung opsi *preset* **100% Tanpa Keyboard** seperti **`Tombol Samping Mouse (Back / XButton1)`** atau **`Ctrl + Win + Klik Kiri`**.
   - **Zero Focus Loss (`WS_EX_NOACTIVATE` + `MA_NOACTIVATE`)**: Baik saat memblok area soal maupun saat membaca/mengklik tombol pada **Floating Stealth Answer HUD**, jendela browser di bawahnya tidak pernah kehilangan fokus (`window.onblur` dan `visibilitychange` tidak pernah terpicu).
2. **Pembacaan Soal Hybrid (Vision Gambar PNG + Teks OCR)**
   - Area soal yang kamu blok otomatis dipotong menjadi gambar `PNG` sekaligus diekstrak teksnya menggunakan *Windows 11 OneOCR / Windows.Media.Ocr*, lalu keduanya dikirim bersama ke AI.
   - Mampu membaca teks soal biasa, tabel, rumus matematika, hingga soal bergambar/diagram murni.
3. **Format Jawaban Adaptif Sesuai Tipe Soal**
   - **Soal Pilihan Ganda:** Langsung menampilkan lencana jawaban tegas (contoh: **`B. Mitokondria`**) di bagian atas kartu *HUD* beserta alasan singkat 1 kalimat.
   - **Soal Esai / Uraian ("Jelaskan..."):** Menampilkan ringkasan inti beserta penjelasan lengkap yang bisa di-*scroll* dengan *mouse wheel* dan langsung disalin ke *Clipboard*.
4. **Input API Key Manual & Fetch Daftar Model Otomatis (`/models`)**
   - Mendukung **Google Gemini**, **OpenAI**, **Groq Cloud**, **OpenRouter**, dan **Custom OpenAI-Compatible Endpoint** (termasuk Ollama lokal).
   - Cukup tempel *API Key* di jendela Pengaturan (`#111111`), klik **`Tembak API & Muat Model`**, maka aplikasi akan menembak endpoint `/models` provider tersebut dan menampilkan daftar model asli yang tersedia untuk dipilih secara bebas.

---

## Cara Menggunakan

1. Jalankan **`AI-Task.exe`** (aplikasi otomatis aktif di *System Tray* Windows dan membuka jendela Pengaturan).
2. Pilih **Provider AI** (misal: `Gemini`, `Groq`, `OpenRouter`, atau `OpenAI`), klik **`Tempel Key`** untuk memasukkan *API Key* kamu, lalu klik **`Tembak API & Muat Model`**.
3. Klik salah satu nama model yang muncul di daftar (atau ketik nama model secara manual).
4. Tekan **`Ctrl + Win + X`** di mana saja, lalu **seret kotak** (`Kotak Area`) atau **seret teks** (`Teks Linear`) pada soal yang ingin dijawab.
5. Begitu klik dilepas, layar kembali normal dan **Floating Stealth Answer HUD** langsung muncul di samping soal menampilkan jawabannya!

---

## Build dari Source (Rust)

```powershell
# 1. Jalankan seluruh unit & integration test
cargo test

# 2. Build binary release
cargo build --release
```

File hasil kompilasi akan tersedia di `target/release/ai-task.exe` dan disalin ke `AI-Task.exe`.
