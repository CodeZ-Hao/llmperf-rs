use unicode_width::UnicodeWidthStr;

/// Calculate display width considering CJK characters
pub fn display_width(s: &str) -> usize {
    UnicodeWidthStr::width(s)
}

/// Pad string to target display width (right-padded)
pub fn pad_right(s: &str, width: usize) -> String {
    let w = display_width(s);
    if w >= width { return s.to_string(); }
    format!("{}{}", s, " ".repeat(width - w))
}

/// Pad string to target display width (left-padded)
pub fn pad_left(s: &str, width: usize) -> String {
    let w = display_width(s);
    if w >= width { return s.to_string(); }
    format!("{}{}", " ".repeat(width - w), s)
}

/// Pad string centered
pub fn pad_center(s: &str, width: usize) -> String {
    let w = display_width(s);
    if w >= width { return s.to_string(); }
    let left = (width - w) / 2;
    let right = width - w - left;
    format!("{}{}{}", " ".repeat(left), s, " ".repeat(right))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_display_width_ascii() {
        assert_eq!(display_width("hello"), 5);
    }

    #[test]
    fn test_display_width_cjk() {
        assert_eq!(display_width("你好"), 4);
    }

    #[test]
    fn test_pad_right() {
        assert_eq!(pad_right("hi", 6), "hi    ");
        assert_eq!(pad_right("toolong", 3), "toolong");
    }

    #[test]
    fn test_pad_left() {
        assert_eq!(pad_left("hi", 6), "    hi");
        assert_eq!(pad_left("toolong", 3), "toolong");
    }

    #[test]
    fn test_pad_center() {
        assert_eq!(pad_center("hi", 6), "  hi  ");
        assert_eq!(pad_center("toolong", 3), "toolong");
    }

    #[test]
    fn test_pad_center_cjk() {
        // "你好" has display width 4, padding to 8
        let result = pad_center("你好", 8);
        assert_eq!(display_width(&result), 8);
    }
}
