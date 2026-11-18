# DESIGN.md - AI-Task Visual & Interaction Direction

## Identity & Purpose
- **Product**: AI-Task (Native Windows Stealth Screen Question Solver)
- **Visual Language**: Matte Carbon Utility (`#111111`), matching the sister utility CopyScreen. Built for zero-distraction operation during study and practice sessions.
- **Dial**: `ENERGY 1 / RHYTHM 2 / MOTION 1`

## Color Palette (3-Tier Neutral Carbon + 1 Functional Accent)
- **Canvas Base**: `#111111` (`rgb(17, 17, 17)`) - primary window and floating HUD background.
- **Elevated Surface**: `#1B1B1B` (`rgb(27, 27, 27)`) - input fields, model cards, and inactive controls.
- **Hover / Selected Surface**: `#252525` / `#232323` with `#EDEDED` active border.
- **Subtle Border / Divider**: `#2D2D2D` / `#262626`.
- **Primary Text & Primary CTA Fill**: `#F2F2F2` (16.8:1 contrast against `#111111`).
- **Secondary Text**: `#A2A2A2` (7.1:1 contrast against `#111111`).
- **Functional Accents**:
  - `#22C55E` (Emerald Green): active toggle state, verified API status, and primary answer badge.
  - `#38BDF8` (Sky Cyan): selection marquee border and active input cursor in the capture overlay.
  - `#EF4444` (Crimson): error state border and exit action.

## Typography
- **UI & HUD Font**: `Segoe UI` (native Windows system font via DirectWrite for zero bundle overhead and crisp ClearType/greyscale rendering).
- **Hierarchy**:
  - Window Title: `18px Bold`
  - Answer Headline (Multiple Choice Badge): `15px Bold`
  - Body / Explanation / Input Value: `13px SemiBold / Regular`
  - Section Labels & Status Notes: `12px SemiBold / Regular`

## Layout & States
- **Dashboard Window (`560 x 720`)**:
  - Structured around the two decisions the user makes: (1) configuring the AI Provider, API Key, and dynamically fetched Model list, and (2) configuring the stealth trigger shortcut and default selection mode.
  - Explicit Empty, Loading, and Error states for the dynamic `/models` API fetch.
- **Floating Stealth Answer HUD (`440 x 290`, `WS_EX_NOACTIVATE`)**:
  - Appears immediately adjacent to the selected question region after mouse release, allowing the rest of the screen to remain unobstructed and clickable without stealing window focus.
