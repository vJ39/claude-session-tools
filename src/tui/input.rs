//! 1 行テキスト入力の編集状態。
//!
//! カーソル位置を文字単位で持ち、途中挿入・削除・左右移動に対応する。
//! 描画 (横スクロールやカーソル座標) は [`crate::tui::ui`] 側で扱う。ここは編集ロジックだけ。

/// 1 行入力の編集状態。`cursor` は文字境界のインデックス (0..=chars)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InputState {
    /// 入力中の文字列
    pub buffer: String,
    /// カーソルの文字位置 (0 = 先頭, chars 数 = 末尾)
    pub cursor: usize,
}

impl InputState {
    /// 空の入力。
    pub fn new() -> Self {
        Self {
            buffer: String::new(),
            cursor: 0,
        }
    }

    /// 初期値つき。カーソルは末尾に置く。
    pub fn with_text(text: impl Into<String>) -> Self {
        let buffer = text.into();
        let cursor = buffer.chars().count();
        Self { buffer, cursor }
    }

    /// 文字数。
    pub fn len(&self) -> usize {
        self.buffer.chars().count()
    }

    pub fn is_empty(&self) -> bool {
        self.buffer.is_empty()
    }

    /// カーソル位置に対応するバイトオフセット。
    fn byte_at(&self, char_idx: usize) -> usize {
        self.buffer
            .char_indices()
            .nth(char_idx)
            .map(|(b, _)| b)
            .unwrap_or(self.buffer.len())
    }

    /// カーソル位置に 1 文字挿入し、カーソルを 1 つ進める。
    pub fn insert(&mut self, c: char) {
        let at = self.byte_at(self.cursor);
        self.buffer.insert(at, c);
        self.cursor += 1;
    }

    /// カーソル位置に文字列をまとめて挿入する (貼り付け用)。カーソルは挿入した分だけ進む。
    pub fn insert_str(&mut self, s: &str) {
        let at = self.byte_at(self.cursor);
        self.buffer.insert_str(at, s);
        self.cursor += s.chars().count();
    }

    /// カーソル直前の 1 文字を削除する (Backspace)。
    pub fn backspace(&mut self) {
        if self.cursor == 0 {
            return;
        }
        let start = self.byte_at(self.cursor - 1);
        let end = self.byte_at(self.cursor);
        self.buffer.replace_range(start..end, "");
        self.cursor -= 1;
    }

    /// カーソル位置の 1 文字を削除する (Delete)。カーソルは動かない。
    pub fn delete(&mut self) {
        if self.cursor >= self.len() {
            return;
        }
        let start = self.byte_at(self.cursor);
        let end = self.byte_at(self.cursor + 1);
        self.buffer.replace_range(start..end, "");
    }

    /// カーソルを 1 つ左へ。
    pub fn left(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
    }

    /// カーソルを 1 つ右へ。
    pub fn right(&mut self) {
        if self.cursor < self.len() {
            self.cursor += 1;
        }
    }

    /// カーソルを先頭へ。
    pub fn home(&mut self) {
        self.cursor = 0;
    }

    /// カーソルを末尾へ。
    pub fn end(&mut self) {
        self.cursor = self.len();
    }

    /// 全消去。
    pub fn clear(&mut self) {
        self.buffer.clear();
        self.cursor = 0;
    }

    /// 前後空白を落とした確定値。
    pub fn trimmed(&self) -> String {
        self.buffer.trim().to_string()
    }
}

impl Default for InputState {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
#[allow(non_snake_case)] // テスト名は日本語で書く
mod tests {
    use super::*;

    #[test]
    fn 初期値つきはカーソルが末尾() {
        let s = InputState::with_text("/tmp");
        assert_eq!(s.buffer, "/tmp");
        assert_eq!(s.cursor, 4);
    }

    #[test]
    fn 末尾に追記() {
        let mut s = InputState::with_text("ab");
        s.insert('c');
        assert_eq!(s.buffer, "abc");
        assert_eq!(s.cursor, 3);
    }

    #[test]
    fn 途中挿入() {
        let mut s = InputState::with_text("ac");
        s.left(); // カーソルは 'c' の前 (index 1)
        s.insert('b');
        assert_eq!(s.buffer, "abc");
        assert_eq!(s.cursor, 2);
    }

    #[test]
    fn backspaceはカーソル直前を消す() {
        let mut s = InputState::with_text("abc");
        s.left(); // index 2 ('c' の前)
        s.backspace(); // 'b' を消す
        assert_eq!(s.buffer, "ac");
        assert_eq!(s.cursor, 1);
    }

    #[test]
    fn 先頭でのbackspaceは何もしない() {
        let mut s = InputState::with_text("abc");
        s.home();
        s.backspace();
        assert_eq!(s.buffer, "abc");
        assert_eq!(s.cursor, 0);
    }

    #[test]
    fn deleteはカーソル位置を消す() {
        let mut s = InputState::with_text("abc");
        s.home();
        s.delete(); // 'a' を消す
        assert_eq!(s.buffer, "bc");
        assert_eq!(s.cursor, 0);
    }

    #[test]
    fn 末尾でのdeleteは何もしない() {
        let mut s = InputState::with_text("abc");
        s.delete();
        assert_eq!(s.buffer, "abc");
        assert_eq!(s.cursor, 3);
    }

    #[test]
    fn 左右移動は範囲内に収まる() {
        let mut s = InputState::with_text("ab");
        s.right(); // 既に末尾
        assert_eq!(s.cursor, 2);
        s.left();
        s.left();
        s.left(); // 先頭を越えない
        assert_eq!(s.cursor, 0);
    }

    #[test]
    fn home_end() {
        let mut s = InputState::with_text("abcd");
        s.home();
        assert_eq!(s.cursor, 0);
        s.end();
        assert_eq!(s.cursor, 4);
    }

    #[test]
    fn 全角文字でも境界を壊さない() {
        let mut s = InputState::with_text("あい");
        s.left(); // 'い' の前
        s.insert('ん');
        assert_eq!(s.buffer, "あんい");
        assert_eq!(s.cursor, 2);
        s.backspace();
        assert_eq!(s.buffer, "あい");
    }

    #[test]
    fn insert_strでまとめて挿入できる() {
        let mut s = InputState::with_text("ac");
        s.left(); // 'c' の前
        s.insert_str("bb");
        assert_eq!(s.buffer, "abbc");
        assert_eq!(s.cursor, 3);
    }

    #[test]
    fn clearで空になる() {
        let mut s = InputState::with_text("abc");
        s.clear();
        assert!(s.is_empty());
        assert_eq!(s.cursor, 0);
    }

    #[test]
    fn trimmedは前後空白を落とす() {
        let s = InputState::with_text("  /tmp  ");
        assert_eq!(s.trimmed(), "/tmp");
    }
}
