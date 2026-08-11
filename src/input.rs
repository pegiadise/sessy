//! Single-line text input with a movable cursor, used by the search bars.
//! The cursor is a byte offset into `text`, always on a char boundary.

#[derive(Debug, Default, Clone)]
pub struct TextInput {
    text: String,
    cursor: usize,
}

impl TextInput {
    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    /// Number of chars before the cursor — the cursor's column for rendering.
    pub fn cursor_chars(&self) -> usize {
        self.text[..self.cursor].chars().count()
    }

    pub fn clear(&mut self) {
        self.text.clear();
        self.cursor = 0;
    }

    pub fn insert(&mut self, c: char) {
        self.text.insert(self.cursor, c);
        self.cursor += c.len_utf8();
    }

    fn prev_boundary(&self) -> Option<usize> {
        self.text[..self.cursor]
            .char_indices()
            .next_back()
            .map(|(i, _)| i)
    }

    fn next_boundary(&self) -> Option<usize> {
        self.text[self.cursor..]
            .chars()
            .next()
            .map(|c| self.cursor + c.len_utf8())
    }

    /// Start of the word before the cursor: skip whitespace, then the word.
    fn word_left_boundary(&self) -> usize {
        self.text[..self.cursor]
            .trim_end_matches(char::is_whitespace)
            .trim_end_matches(|c: char| !c.is_whitespace())
            .len()
    }

    /// End of the word after the cursor: skip whitespace, then the word.
    fn word_right_boundary(&self) -> usize {
        let remaining = self.text[self.cursor..]
            .trim_start_matches(char::is_whitespace)
            .trim_start_matches(|c: char| !c.is_whitespace())
            .len();
        self.text.len() - remaining
    }

    pub fn move_left(&mut self) {
        if let Some(i) = self.prev_boundary() {
            self.cursor = i;
        }
    }

    pub fn move_right(&mut self) {
        if let Some(i) = self.next_boundary() {
            self.cursor = i;
        }
    }

    pub fn move_word_left(&mut self) {
        self.cursor = self.word_left_boundary();
    }

    pub fn move_word_right(&mut self) {
        self.cursor = self.word_right_boundary();
    }

    pub fn move_home(&mut self) {
        self.cursor = 0;
    }

    pub fn move_end(&mut self) {
        self.cursor = self.text.len();
    }

    pub fn backspace(&mut self) {
        if let Some(i) = self.prev_boundary() {
            self.text.remove(i);
            self.cursor = i;
        }
    }

    pub fn delete_forward(&mut self) {
        if self.cursor < self.text.len() {
            self.text.remove(self.cursor);
        }
    }

    pub fn delete_word_backwards(&mut self) {
        let start = self.word_left_boundary();
        self.text.replace_range(start..self.cursor, "");
        self.cursor = start;
    }

    pub fn delete_to_start(&mut self) {
        self.text.replace_range(..self.cursor, "");
        self.cursor = 0;
    }

    pub fn delete_to_end(&mut self) {
        self.text.truncate(self.cursor);
    }
}

impl From<&str> for TextInput {
    fn from(s: &str) -> Self {
        Self {
            cursor: s.len(),
            text: s.to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_str_puts_cursor_at_end() {
        let input = TextInput::from("hello");
        assert_eq!(input.text(), "hello");
        assert_eq!(input.cursor_chars(), 5);
    }

    #[test]
    fn insert_mid_string_after_moving_left() {
        let mut input = TextInput::from("helo");
        input.move_left();
        input.insert('l');
        assert_eq!(input.text(), "hello");
        assert_eq!(input.cursor_chars(), 4);
    }

    #[test]
    fn backspace_mid_string() {
        let mut input = TextInput::from("heello");
        input.move_left();
        input.move_left();
        input.move_left();
        input.backspace();
        assert_eq!(input.text(), "hello");
        assert_eq!(input.cursor_chars(), 2);
    }

    #[test]
    fn delete_forward_removes_char_at_cursor() {
        let mut input = TextInput::from("hello");
        input.move_home();
        input.delete_forward();
        assert_eq!(input.text(), "ello");
        assert_eq!(input.cursor_chars(), 0);
    }

    #[test]
    fn arrows_clamp_at_edges() {
        let mut input = TextInput::from("ab");
        input.move_right(); // already at end
        assert_eq!(input.cursor_chars(), 2);
        input.move_home();
        input.move_left(); // already at start
        assert_eq!(input.cursor_chars(), 0);
    }

    #[test]
    fn word_moves_jump_over_words() {
        let mut input = TextInput::from("hello brave world");
        input.move_word_left();
        assert_eq!(input.cursor_chars(), 12); // start of "world"
        input.move_word_left();
        assert_eq!(input.cursor_chars(), 6); // start of "brave"
        input.move_word_right();
        assert_eq!(input.cursor_chars(), 11); // end of "brave"
        input.move_word_right();
        assert_eq!(input.cursor_chars(), 17); // end of "world"
    }

    #[test]
    fn delete_word_backwards_strips_trailing_space_then_word() {
        // Readline behavior: skip trailing whitespace, then delete the word,
        // leaving the space *before* it intact.
        let mut input = TextInput::from("hello world ");
        input.delete_word_backwards();
        assert_eq!(input.text(), "hello ");
    }

    #[test]
    fn delete_word_backwards_no_trailing_space() {
        let mut input = TextInput::from("hello world");
        input.delete_word_backwards();
        assert_eq!(input.text(), "hello ");
    }

    #[test]
    fn delete_word_backwards_only_one_word() {
        let mut input = TextInput::from("hello");
        input.delete_word_backwards();
        assert_eq!(input.text(), "");
    }

    #[test]
    fn delete_word_backwards_empty_is_noop() {
        let mut input = TextInput::default();
        input.delete_word_backwards();
        assert_eq!(input.text(), "");
    }

    #[test]
    fn delete_word_backwards_mid_string_keeps_tail() {
        let mut input = TextInput::from("hello brave world");
        input.move_word_left(); // cursor at start of "world"
        input.delete_word_backwards(); // deletes "brave "
        assert_eq!(input.text(), "hello world");
        assert_eq!(input.cursor_chars(), 6);
    }

    #[test]
    fn delete_to_start_keeps_tail_after_cursor() {
        let mut input = TextInput::from("hello world");
        input.move_word_left(); // cursor at start of "world"
        input.delete_to_start();
        assert_eq!(input.text(), "world");
        assert_eq!(input.cursor_chars(), 0);
    }

    #[test]
    fn delete_to_end_keeps_head_before_cursor() {
        let mut input = TextInput::from("hello world");
        input.move_word_left();
        input.delete_to_end();
        assert_eq!(input.text(), "hello ");
    }

    #[test]
    fn handles_unicode() {
        let mut input = TextInput::from("καλημέρα κόσμε");
        input.delete_word_backwards();
        assert_eq!(input.text(), "καλημέρα ");
        input.move_left(); // cursor before the trailing space
        input.backspace(); // removes the 'α'
        assert_eq!(input.text(), "καλημέρ ");
        input.insert('ν');
        assert_eq!(input.text(), "καλημέρν ");
    }
}
