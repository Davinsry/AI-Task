#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct RectF {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

impl RectF {
    pub fn new(x: f32, y: f32, width: f32, height: f32) -> Self {
        Self {
            x,
            y,
            width: width.max(0.0),
            height: height.max(0.0),
        }
    }

    pub fn from_points(x1: f32, y1: f32, x2: f32, y2: f32) -> Self {
        let min_x = x1.min(x2);
        let min_y = y1.min(y2);
        let max_x = x1.max(x2);
        let max_y = y1.max(y2);
        Self::new(min_x, min_y, max_x - min_x, max_y - min_y)
    }

    pub fn right(&self) -> f32 {
        self.x + self.width
    }

    pub fn bottom(&self) -> f32 {
        self.y + self.height
    }

    pub fn center_x(&self) -> f32 {
        self.x + self.width * 0.5
    }

    pub fn center_y(&self) -> f32 {
        self.y + self.height * 0.5
    }

    pub fn contains(&self, px: f32, py: f32) -> bool {
        px >= self.x && px <= self.right() && py >= self.y && py <= self.bottom()
    }

    pub fn contains_padded(&self, px: f32, py: f32, pad_x: f32, pad_y: f32) -> bool {
        px >= (self.x - pad_x)
            && px <= (self.right() + pad_x)
            && py >= (self.y - pad_y)
            && py <= (self.bottom() + pad_y)
    }

    pub fn intersection_area(&self, other: &RectF) -> f32 {
        let ix1 = self.x.max(other.x);
        let iy1 = self.y.max(other.y);
        let ix2 = self.right().min(other.right());
        let iy2 = self.bottom().min(other.bottom());
        if ix2 > ix1 && iy2 > iy1 {
            (ix2 - ix1) * (iy2 - iy1)
        } else {
            0.0
        }
    }

    pub fn area(&self) -> f32 {
        self.width * self.height
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct DetectedWord {
    pub text: String,
    pub bounds: RectF,
    pub line_index: usize,
    pub word_index_in_line: usize,
    pub global_index: usize,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DetectedLine {
    pub words: Vec<DetectedWord>,
    pub bounds: RectF,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelectionMode {
    /// Select all content inside a rectangular region (default for AI-Task: captures both image region & OCR words).
    Box,
    /// Select words in natural reading order from start point to end point (like browser/PDF text selection).
    Linear,
}

impl Default for SelectionMode {
    fn default() -> Self {
        Self::Box
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct SelectionResult {
    /// Extracted OCR text within the selection (may be empty if the user boxed a pure diagram/graph).
    pub text: String,
    /// Visual highlight rectangles to draw on the overlay.
    pub highlight_rects: Vec<RectF>,
    /// Global word indices that are part of the current selection.
    pub selected_word_indices: Vec<usize>,
    /// Bounding rectangle on screen to crop for Hybrid Vision + OCR AI input.
    pub crop_bounds: Option<RectF>,
}

impl SelectionResult {
    /// Returns true if the user has selected either OCR words or a valid image box (`>= 14x14` px).
    pub fn has_valid_target(&self) -> bool {
        if !self.text.trim().is_empty() {
            return true;
        }
        if let Some(b) = self.crop_bounds {
            return b.width >= 14.0 && b.height >= 14.0;
        }
        false
    }
}

#[derive(Debug, Clone, Default)]
pub struct OcrLayout {
    pub lines: Vec<DetectedLine>,
    pub words: Vec<DetectedWord>,
}

impl OcrLayout {
    pub fn from_raw_lines(raw_lines: Vec<Vec<(String, RectF)>>) -> Self {
        let mut temp_lines: Vec<(RectF, Vec<(String, RectF)>)> = Vec::new();

        for mut line_words in raw_lines {
            line_words.retain(|(t, b)| !t.trim().is_empty() && b.width > 0.0 && b.height > 0.0);
            if line_words.is_empty() {
                continue;
            }
            line_words.sort_by(|a, b| {
                a.1.x
                    .partial_cmp(&b.1.x)
                    .unwrap_or(std::cmp::Ordering::Equal)
            });

            let mut current_seg: Vec<(String, RectF)> = Vec::new();
            for (text, rect) in line_words {
                if let Some((_, prev_rect)) = current_seg.last() {
                    let min_h = prev_rect.height.min(rect.height).max(10.0);
                    let max_col_gap = (min_h * 1.8).clamp(16.0, 38.0);
                    if (rect.x - prev_rect.right()) > max_col_gap {
                        let seg_bounds = compute_union_bounds(current_seg.iter().map(|(_, r)| *r));
                        temp_lines.push((seg_bounds, std::mem::take(&mut current_seg)));
                    }
                }
                current_seg.push((text, rect));
            }
            if !current_seg.is_empty() {
                let seg_bounds = compute_union_bounds(current_seg.iter().map(|(_, r)| *r));
                temp_lines.push((seg_bounds, current_seg));
            }
        }

        temp_lines.sort_by(|(a, _), (b, _)| {
            let vertical_overlap = (a.bottom().min(b.bottom()) - a.y.max(b.y)).max(0.0);
            let min_h = a.height.min(b.height).max(1.0);
            if vertical_overlap / min_h > 0.5 {
                a.x.partial_cmp(&b.x).unwrap_or(std::cmp::Ordering::Equal)
            } else {
                a.center_y()
                    .partial_cmp(&b.center_y())
                    .unwrap_or(std::cmp::Ordering::Equal)
            }
        });

        let mut lines = Vec::with_capacity(temp_lines.len());
        let mut all_words = Vec::new();
        let mut global_index = 0;

        for (line_index, (line_bounds, raw_words)) in temp_lines.into_iter().enumerate() {
            let mut detected_words = Vec::with_capacity(raw_words.len());
            for (word_index_in_line, (text, bounds)) in raw_words.into_iter().enumerate() {
                let word = DetectedWord {
                    text,
                    bounds,
                    line_index,
                    word_index_in_line,
                    global_index,
                };
                global_index += 1;
                all_words.push(word.clone());
                detected_words.push(word);
            }
            lines.push(DetectedLine {
                words: detected_words,
                bounds: line_bounds,
            });
        }

        Self {
            lines,
            words: all_words,
        }
    }

    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.words.is_empty()
    }

    pub fn word_at_point(&self, px: f32, py: f32) -> Option<&DetectedWord> {
        for word in &self.words {
            let pad_y = (word.bounds.height * 0.25).clamp(2.0, 8.0);
            let pad_x = (word.bounds.height * 0.35).clamp(3.0, 10.0);
            if word.bounds.contains_padded(px, py, pad_x, pad_y) {
                return Some(word);
            }
        }
        None
    }

    pub fn full_text(&self) -> String {
        let mut out = String::new();
        for (i, line) in self.lines.iter().enumerate() {
            if i > 0 {
                out.push('\n');
            }
            for (j, word) in line.words.iter().enumerate() {
                if j > 0 {
                    out.push(' ');
                }
                out.push_str(&word.text);
            }
        }
        out
    }

    pub fn compute_selection(
        &self,
        x1: f32,
        y1: f32,
        x2: f32,
        y2: f32,
        mode: SelectionMode,
    ) -> SelectionResult {
        let drag_dist_sq = (x2 - x1).powi(2) + (y2 - y1).powi(2);
        if drag_dist_sq < 9.0 {
            if let Some(word) = self.word_at_point(x1, y1) {
                return SelectionResult {
                    text: word.text.clone(),
                    highlight_rects: vec![word.bounds],
                    selected_word_indices: vec![word.global_index],
                    crop_bounds: Some(word.bounds),
                };
            }
            return SelectionResult::default();
        }

        match mode {
            SelectionMode::Linear => self.compute_linear_selection(x1, y1, x2, y2),
            SelectionMode::Box => self.compute_box_selection(x1, y1, x2, y2),
        }
    }

    fn compute_linear_selection(&self, x1: f32, y1: f32, x2: f32, y2: f32) -> SelectionResult {
        if self.words.is_empty() {
            return self.compute_box_selection(x1, y1, x2, y2);
        }

        let start_idx = self.find_reading_anchor(x1, y1);
        let end_idx = self.find_reading_anchor(x2, y2);

        let (Some(a), Some(b)) = (start_idx, end_idx) else {
            return self.compute_box_selection(x1, y1, x2, y2);
        };

        if a == b {
            let word = &self.words[a];
            if word.bounds.contains_padded(x1, y1, 8.0, 8.0)
                && word.bounds.contains_padded(x2, y2, 8.0, 8.0)
            {
                return slice_single_word(word, x1, x2);
            }
        }

        let (first, last) = if a <= b { (a, b) } else { (b, a) };
        let selected: Vec<&DetectedWord> = self.words[first..=last].iter().collect();
        let mut res = build_selection_from_words(&selected);
        let union = compute_union_bounds(res.highlight_rects.iter().copied());
        if union.width > 0.0 && union.height > 0.0 {
            res.crop_bounds = Some(union);
        }
        res
    }

    fn compute_box_selection(&self, x1: f32, y1: f32, x2: f32, y2: f32) -> SelectionResult {
        let drag_rect = RectF::from_points(x1, y1, x2, y2);
        let mut selected = Vec::new();

        for word in &self.words {
            let center_in = drag_rect.contains(word.bounds.center_x(), word.bounds.center_y());
            let overlap_ratio = if word.bounds.area() > 0.0 {
                word.bounds.intersection_area(&drag_rect) / word.bounds.area()
            } else {
                0.0
            };

            if center_in || overlap_ratio >= 0.45 {
                selected.push(word);
            }
        }

        let mut res = build_selection_from_words(&selected);
        // Always preserve the user's drawn box as crop_bounds (expanded slightly if words stick out)
        let union = if res.highlight_rects.is_empty() {
            drag_rect
        } else {
            compute_union_bounds(
                res.highlight_rects
                    .iter()
                    .copied()
                    .chain(std::iter::once(drag_rect)),
            )
        };
        if union.width >= 4.0 && union.height >= 4.0 {
            res.crop_bounds = Some(union);
        }
        res
    }

    fn find_reading_anchor(&self, px: f32, py: f32) -> Option<usize> {
        if let Some(exact) = self.word_at_point(px, py) {
            return Some(exact.global_index);
        }

        let mut best_line_idx = None;
        let mut best_vertical_dist = f32::MAX;

        for (idx, line) in self.lines.iter().enumerate() {
            let pad_y = (line.bounds.height * 0.6).clamp(6.0, 28.0);
            let dist_y = if py < line.bounds.y {
                line.bounds.y - py
            } else if py > line.bounds.bottom() {
                py - line.bounds.bottom()
            } else {
                0.0
            };

            if dist_y <= pad_y && dist_y < best_vertical_dist {
                best_vertical_dist = dist_y;
                best_line_idx = Some(idx);
            }
        }

        if let Some(line_idx) = best_line_idx {
            let line = &self.lines[line_idx];
            let mut best_word = None;
            let mut best_dx = f32::MAX;
            for word in &line.words {
                let dx = if px < word.bounds.x {
                    word.bounds.x - px
                } else if px > word.bounds.right() {
                    px - word.bounds.right()
                } else {
                    0.0
                };
                if dx < best_dx {
                    best_dx = dx;
                    best_word = Some(word.global_index);
                }
            }
            if best_word.is_some() {
                return best_word;
            }
        }

        let max_dist_sq = 96.0_f32.powi(2);
        let mut best_idx = None;
        let mut best_dist_sq = max_dist_sq;
        for word in &self.words {
            let anchor_x = px.clamp(word.bounds.x, word.bounds.right());
            let anchor_y = py.clamp(word.bounds.y, word.bounds.bottom());
            let d2 = (px - anchor_x).powi(2) + (py - anchor_y).powi(2);
            if d2 <= best_dist_sq {
                best_dist_sq = d2;
                best_idx = Some(word.global_index);
            }
        }
        best_idx
    }
}

fn slice_single_word(word: &DetectedWord, x1: f32, x2: f32) -> SelectionResult {
    let chars: Vec<char> = word.text.chars().collect();
    let char_count = chars.len();
    if char_count <= 1 || word.bounds.width <= 1.0 {
        return SelectionResult {
            text: word.text.clone(),
            highlight_rects: vec![word.bounds],
            selected_word_indices: vec![word.global_index],
            crop_bounds: Some(word.bounds),
        };
    }

    let min_x = x1.min(x2).clamp(word.bounds.x, word.bounds.right());
    let max_x = x1.max(x2).clamp(word.bounds.x, word.bounds.right());
    let span_ratio = (max_x - min_x) / word.bounds.width;

    if span_ratio >= 0.80 {
        return SelectionResult {
            text: word.text.clone(),
            highlight_rects: vec![word.bounds],
            selected_word_indices: vec![word.global_index],
            crop_bounds: Some(word.bounds),
        };
    }

    let rel_start = ((min_x - word.bounds.x) / word.bounds.width).clamp(0.0, 1.0);
    let rel_end = ((max_x - word.bounds.x) / word.bounds.width).clamp(0.0, 1.0);

    let start_char = ((rel_start * char_count as f32).floor() as usize).min(char_count - 1);
    let end_char =
        ((rel_end * char_count as f32).ceil() as usize).clamp(start_char + 1, char_count);

    let sliced_text: String = chars[start_char..end_char].iter().collect();
    let char_w = word.bounds.width / char_count as f32;
    let sub_rect = RectF::new(
        word.bounds.x + start_char as f32 * char_w,
        word.bounds.y,
        (end_char - start_char) as f32 * char_w,
        word.bounds.height,
    );

    SelectionResult {
        text: sliced_text,
        highlight_rects: vec![sub_rect],
        selected_word_indices: vec![word.global_index],
        crop_bounds: Some(word.bounds),
    }
}

fn build_selection_from_words(words: &[&DetectedWord]) -> SelectionResult {
    if words.is_empty() {
        return SelectionResult::default();
    }

    let mut text = String::new();
    let mut highlight_rects = Vec::new();
    let mut selected_word_indices = Vec::with_capacity(words.len());

    let mut current_line: Option<usize> = None;
    let mut current_line_rect: Option<RectF> = None;

    for word in words {
        selected_word_indices.push(word.global_index);

        match current_line {
            None => {
                text.push_str(&word.text);
                current_line = Some(word.line_index);
                current_line_rect = Some(word.bounds);
            }
            Some(prev_line) if prev_line == word.line_index => {
                text.push(' ');
                text.push_str(&word.text);
                if let Some(r) = current_line_rect.as_mut() {
                    if (word.bounds.x - r.right()) <= r.height * 2.5 {
                        let min_x = r.x.min(word.bounds.x);
                        let min_y = r.y.min(word.bounds.y);
                        let max_x = r.right().max(word.bounds.right());
                        let max_y = r.bottom().max(word.bounds.bottom());
                        *r = RectF::new(min_x, min_y, max_x - min_x, max_y - min_y);
                    } else {
                        highlight_rects.push(*r);
                        *r = word.bounds;
                    }
                }
            }
            Some(_) => {
                if let Some(r) = current_line_rect.take() {
                    highlight_rects.push(r);
                }
                text.push('\n');
                text.push_str(&word.text);
                current_line = Some(word.line_index);
                current_line_rect = Some(word.bounds);
            }
        }
    }

    if let Some(r) = current_line_rect {
        highlight_rects.push(r);
    }

    SelectionResult {
        text,
        highlight_rects,
        selected_word_indices,
        crop_bounds: None,
    }
}

pub fn compute_union_bounds(rects: impl Iterator<Item = RectF>) -> RectF {
    let mut min_x = f32::MAX;
    let mut min_y = f32::MAX;
    let mut max_x = f32::MIN;
    let mut max_y = f32::MIN;
    let mut any = false;

    for r in rects {
        any = true;
        min_x = min_x.min(r.x);
        min_y = min_y.min(r.y);
        max_x = max_x.max(r.right());
        max_y = max_y.max(r.bottom());
    }

    if !any {
        RectF::new(0.0, 0.0, 0.0, 0.0)
    } else {
        RectF::new(min_x, min_y, max_x - min_x, max_y - min_y)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_layout() -> OcrLayout {
        let line1 = vec![
            ("Manakah".to_string(), RectF::new(100.0, 50.0, 65.0, 20.0)),
            ("organel".to_string(), RectF::new(172.0, 50.0, 55.0, 20.0)),
            ("penghasil".to_string(), RectF::new(234.0, 50.0, 68.0, 20.0)),
            ("ATP?".to_string(), RectF::new(309.0, 50.0, 38.0, 20.0)),
        ];
        let line2 = vec![
            ("A.".to_string(), RectF::new(100.0, 90.0, 20.0, 20.0)),
            ("Ribosom".to_string(), RectF::new(126.0, 90.0, 62.0, 20.0)),
            ("B.".to_string(), RectF::new(200.0, 90.0, 20.0, 20.0)),
            ("Mitokondria".to_string(), RectF::new(226.0, 90.0, 88.0, 20.0)),
        ];
        OcrLayout::from_raw_lines(vec![line2, line1])
    }

    #[test]
    fn box_selection_captures_both_text_and_crop_bounds() {
        let layout = sample_layout();
        let res = layout.compute_selection(90.0, 40.0, 350.0, 120.0, SelectionMode::Box);
        assert_eq!(
            res.text,
            "Manakah organel penghasil ATP?\nA. Ribosom B. Mitokondria"
        );
        assert!(res.has_valid_target());
        let crop = res.crop_bounds.expect("Crop bounds should be present");
        assert!(crop.width >= 250.0 && crop.height >= 70.0);
    }

    #[test]
    fn box_selection_on_pure_image_area_without_text_is_still_valid_target() {
        let layout = OcrLayout::default();
        let res = layout.compute_selection(50.0, 50.0, 250.0, 200.0, SelectionMode::Box);
        assert!(res.text.is_empty());
        assert!(res.has_valid_target());
        assert_eq!(res.crop_bounds, Some(RectF::new(50.0, 50.0, 200.0, 150.0)));
    }
}
