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

pub fn text_width(text: &str, size_pt: f32) -> f32 {
    text.chars().map(|c| char_width(c, size_pt)).sum()
}

pub fn wrap_text(text: &str, size_pt: f32, max_width: f32) -> Vec<String> {
    if text.is_empty() {
        return vec![String::new()];
    }
    let mut lines = Vec::new();
    for segment in text.split('\n') {
        if segment.is_empty() {
            lines.push(String::new());
            continue;
        }
        let mut current_line = String::new();
        let mut current_width = 0.0_f32;
        let space_width = char_width(' ', size_pt);

        for word in segment.split(' ') {
            if word.is_empty() {
                continue;
            }
            let word_w = text_width(word, size_pt);
            if current_line.is_empty() {
                current_line.push_str(word);
                current_width = word_w;
            } else if current_width + space_width + word_w <= max_width {
                current_line.push(' ');
                current_line.push_str(word);
                current_width += space_width + word_w;
            } else {
                lines.push(current_line);
                current_line = word.to_owned();
                current_width = word_w;
            }
        }
        if !current_line.is_empty() {
            lines.push(current_line);
        }
    }
    if lines.is_empty() {
        lines.push(String::new());
    }
    lines
}
