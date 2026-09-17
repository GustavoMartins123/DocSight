pub fn char_width(c: char, size_pt: f32) -> f32 {
    let units = match c {
        ' ' => 250,
        '0'..='9' => 500,
        'i' | 'l' => 222,
        'f' | 'j' | 'r' | 't' => 333,
        'm' => 833,
        'w' => 722,
        'a'..='z' => 500,
        'I' => 278,
        'J' => 333,
        'M' => 833,
        'W' => 944,
        'B' | 'E' | 'F' | 'L' | 'P' | 'S' | 'Z' => 556,
        'A'..='Z' => 667,
        '.' | ',' | ':' | ';' | '!' | '\'' => 222,
        '(' | ')' | '[' | ']' | '-' | '_' | '/' | '\\' => 333,
        '?' | '"' => 444,
        _ => 500,
    };
    (units as f32 / 1000.0) * size_pt
}

pub fn font_fingerprint() -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(b"docsight-layout-font-table-v1");
    for code in 32_u32..127 {
        let character = char::from_u32(code);
        hasher.update(code.to_le_bytes());
        if let Some(character) = character {
            hasher.update(char_width(character, 1000.0).to_le_bytes());
        }
    }
    let digest = hasher.finalize();
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

pub fn text_width(text: &str, size_pt: f32) -> f32 {
    text.chars().map(|c| char_width(c, size_pt)).sum()
}

#[derive(Clone, Debug, PartialEq)]
pub struct WrappedLine {
    pub text: String,
    pub start_char: usize,
}

pub fn wrap_text(text: &str, size_pt: f32, max_width: f32) -> Vec<String> {
    wrap_text_indexed(text, size_pt, max_width, max_width)
        .into_iter()
        .map(|line| line.text)
        .collect()
}

pub fn wrap_text_indexed(
    text: &str,
    size_pt: f32,
    first_line_width: f32,
    line_width: f32,
) -> Vec<WrappedLine> {
    let space_width = char_width(' ', size_pt);
    let mut lines: Vec<WrappedLine> = Vec::new();
    let mut segment_start = 0_usize;
    for segment in text.split('\n') {
        let segment_chars = segment.chars().count();
        if segment.is_empty() {
            lines.push(WrappedLine {
                text: String::new(),
                start_char: segment_start,
            });
            segment_start += 1;
            continue;
        }
        let mut current = String::new();
        let mut current_width = 0.0_f32;
        let mut current_start = segment_start;
        let mut word_start = segment_start;
        for word in segment.split(' ') {
            let word_chars = word.chars().count();
            if !word.is_empty() {
                let word_width = text_width(word, size_pt);
                let limit = if lines.is_empty() {
                    first_line_width
                } else {
                    line_width
                };
                if current.is_empty() {
                    current.push_str(word);
                    current_width = word_width;
                    current_start = word_start;
                } else if current_width + space_width + word_width <= limit {
                    current.push(' ');
                    current.push_str(word);
                    current_width += space_width + word_width;
                } else {
                    lines.push(WrappedLine {
                        text: std::mem::take(&mut current),
                        start_char: current_start,
                    });
                    current.push_str(word);
                    current_width = word_width;
                    current_start = word_start;
                }
            }
            word_start += word_chars + 1;
        }
        if !current.is_empty() {
            lines.push(WrappedLine {
                text: current,
                start_char: current_start,
            });
        }
        segment_start += segment_chars + 1;
    }
    if lines.is_empty() {
        lines.push(WrappedLine {
            text: String::new(),
            start_char: 0,
        });
    }
    lines
}
