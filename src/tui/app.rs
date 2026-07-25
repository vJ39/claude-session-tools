//! TUI の状態遷移。描画とプロセス起動は含めず、ここだけで単体テストできるようにしてある。
//!
//! キー入力を受けて状態を更新し、外側でやってほしい副作用を [`Effect`] として返す。

use std::collections::HashSet;

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::filter::Filter;
use crate::rows::SessionRow;

/// 確認ダイアログの種類。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfirmKind {
    Delete,
    Archive,
}

/// 文字入力プロンプトの種類。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputKind {
    /// jsonl 全文検索
    Grep,
    /// タグ付け
    Tag,
}

/// 画面のモード。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Mode {
    Normal,
    Confirm {
        kind: ConfirmKind,
        message: String,
    },
    Input {
        kind: InputKind,
        prompt: String,
        buffer: String,
    },
}

/// 外側 (イベントループ) に実行してもらう副作用。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Effect {
    None,
    Quit,
    /// 行インデックス (rows 側の添字)
    Resume(usize),
    Delete(usize),
    Archive(usize),
    Recap(usize),
    /// jsonl 全文検索の実行
    Grep(String),
    AddTag(usize, String),
    ClearTags(usize),
    /// サブエージェント記録の表示/非表示を切り替える (再走査が要るので外側に委ねる)
    ToggleSubagents,
}

/// TUI の状態。
pub struct App {
    /// 全行 (作成日時降順)
    pub rows: Vec<SessionRow>,
    /// 絞り込み後の rows 添字
    pub filtered: Vec<usize>,
    /// fuzzy クエリ
    pub query: String,
    /// filtered 内のカーソル位置
    pub cursor: usize,
    pub mode: Mode,
    /// 画面下部に出すメッセージ
    pub status: Option<String>,
    /// 内容検索でヒットした sessionId (None なら未実行)
    pub content_hits: Option<HashSet<String>>,
    /// 内容検索に使ったキーワード (表示用)
    pub grep_query: Option<String>,
    /// サブエージェント記録を表示しているか。
    ///
    /// 表示用のフラグに過ぎず、実際の再走査 (jsonl を読み直す) は
    /// [`Effect::ToggleSubagents`] を受けた外側 (イベントループ) が行う。
    /// 起動時の初期値は CLI オプション `--include-subagents` から呼び出し側が設定する。
    pub include_subagents: bool,
    filter: Filter,
}

impl App {
    pub fn new(rows: Vec<SessionRow>) -> Self {
        let mut app = Self {
            rows,
            filtered: Vec::new(),
            query: String::new(),
            cursor: 0,
            mode: Mode::Normal,
            status: None,
            content_hits: None,
            grep_query: None,
            include_subagents: false,
            filter: Filter::new(),
        };
        app.refilter();
        app
    }

    /// クエリと内容検索結果で絞り込み直す。カーソルは範囲内に丸める。
    pub fn refilter(&mut self) {
        self.filtered = self
            .filter
            .apply(&self.rows, &self.query, self.content_hits.as_ref());
        self.clamp_cursor();
    }

    fn clamp_cursor(&mut self) {
        if self.filtered.is_empty() {
            self.cursor = 0;
        } else if self.cursor >= self.filtered.len() {
            self.cursor = self.filtered.len() - 1;
        }
    }

    /// いま選択している行の rows 添字。
    pub fn selected_index(&self) -> Option<usize> {
        self.filtered.get(self.cursor).copied()
    }

    /// いま選択している行。
    pub fn selected(&self) -> Option<&SessionRow> {
        self.selected_index().map(|i| &self.rows[i])
    }

    /// 表示件数 / 全件数。
    pub fn counts(&self) -> (usize, usize) {
        (self.filtered.len(), self.rows.len())
    }

    /// カーソル移動 (端で止まる)。
    pub fn move_cursor(&mut self, delta: isize) {
        if self.filtered.is_empty() {
            self.cursor = 0;
            return;
        }
        let last = self.filtered.len() as isize - 1;
        let next = (self.cursor as isize + delta).clamp(0, last);
        self.cursor = next as usize;
    }

    /// 状態メッセージを差し替える。
    pub fn set_status(&mut self, msg: impl Into<String>) {
        self.status = Some(msg.into());
    }

    /// 削除・アーカイブ後に行を取り除く。
    pub fn remove_row(&mut self, index: usize) {
        if index >= self.rows.len() {
            return;
        }
        self.rows.remove(index);
        self.refilter();
    }

    /// 全行を入れ替える (サブエージェント表示切替などデータの再取得後に使う)。
    ///
    /// 選択中の session_id が新しい一覧にも残っていれば、そこへカーソルを合わせ直す。
    /// 見つからなければ [`Self::refilter`] によるクランプ済みの位置のままにする。
    pub fn replace_rows(&mut self, rows: Vec<SessionRow>) {
        let keep = self.selected().map(|r| r.session_id.clone());
        self.rows = rows;
        self.refilter();
        if let Some(id) = keep
            && let Some(pos) = self.filtered.iter().position(|&i| self.rows[i].session_id == id)
        {
            self.cursor = pos;
        }
    }

    /// 内容検索の結果を適用する。
    pub fn set_content_hits(&mut self, needle: String, hits: HashSet<String>) {
        let n = hits.len();
        self.content_hits = Some(hits);
        self.grep_query = Some(needle.clone());
        self.cursor = 0;
        self.refilter();
        self.set_status(format!("内容検索「{needle}」: {n} セッション"));
    }

    /// 内容検索の絞り込みを解除する。
    pub fn clear_content_hits(&mut self) {
        self.content_hits = None;
        self.grep_query = None;
        self.refilter();
    }

    /// タグ更新を行に反映する。
    pub fn set_tags(&mut self, index: usize, tags: Vec<String>) {
        if let Some(row) = self.rows.get_mut(index) {
            row.set_tags(tags);
        }
        self.refilter();
    }

    /// キー入力を処理する。
    pub fn on_key(&mut self, key: KeyEvent) -> Effect {
        match self.mode.clone() {
            Mode::Normal => self.on_key_normal(key),
            Mode::Confirm { kind, .. } => self.on_key_confirm(key, kind),
            Mode::Input { kind, prompt, buffer } => self.on_key_input(key, kind, prompt, buffer),
        }
    }

    fn on_key_normal(&mut self, key: KeyEvent) -> Effect {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);

        if ctrl {
            return match key.code {
                KeyCode::Char('c') => Effect::Quit,
                KeyCode::Char('n') => {
                    self.move_cursor(1);
                    Effect::None
                }
                KeyCode::Char('p') => {
                    self.move_cursor(-1);
                    Effect::None
                }
                KeyCode::Char('u') => {
                    self.query.clear();
                    self.refilter();
                    Effect::None
                }
                KeyCode::Char('d') => self.begin_confirm(ConfirmKind::Delete),
                KeyCode::Char('a') => self.begin_confirm(ConfirmKind::Archive),
                KeyCode::Char('g') => {
                    self.mode = Mode::Input {
                        kind: InputKind::Grep,
                        prompt: "内容検索".to_string(),
                        buffer: String::new(),
                    };
                    Effect::None
                }
                KeyCode::Char('t') => {
                    if self.selected_index().is_none() {
                        return Effect::None;
                    }
                    self.mode = Mode::Input {
                        kind: InputKind::Tag,
                        prompt: "タグ (空 Enter で全解除)".to_string(),
                        buffer: String::new(),
                    };
                    Effect::None
                }
                KeyCode::Char('r') => match self.selected_index() {
                    Some(i) => Effect::Recap(i),
                    None => Effect::None,
                },
                KeyCode::Char('s') => Effect::ToggleSubagents,
                _ => Effect::None,
            };
        }

        match key.code {
            KeyCode::Esc => {
                if !self.query.is_empty() {
                    self.query.clear();
                    self.refilter();
                    Effect::None
                } else if self.content_hits.is_some() {
                    self.clear_content_hits();
                    self.set_status("内容検索を解除した");
                    Effect::None
                } else {
                    Effect::Quit
                }
            }
            KeyCode::Enter => match self.selected_index() {
                Some(i) => Effect::Resume(i),
                None => Effect::None,
            },
            KeyCode::Down => {
                self.move_cursor(1);
                Effect::None
            }
            KeyCode::Up => {
                self.move_cursor(-1);
                Effect::None
            }
            KeyCode::PageDown => {
                self.move_cursor(10);
                Effect::None
            }
            KeyCode::PageUp => {
                self.move_cursor(-10);
                Effect::None
            }
            KeyCode::Home => {
                self.cursor = 0;
                Effect::None
            }
            KeyCode::End => {
                self.cursor = self.filtered.len().saturating_sub(1);
                Effect::None
            }
            KeyCode::Backspace => {
                self.query.pop();
                self.cursor = 0;
                self.refilter();
                Effect::None
            }
            KeyCode::Char(c) => {
                self.query.push(c);
                self.cursor = 0;
                self.refilter();
                Effect::None
            }
            _ => Effect::None,
        }
    }

    fn begin_confirm(&mut self, kind: ConfirmKind) -> Effect {
        let row = match self.selected() {
            Some(r) => r,
            None => return Effect::None,
        };
        let verb = match kind {
            ConfirmKind::Delete => "削除",
            ConfirmKind::Archive => "アーカイブ",
        };
        // 実行中セッションは警告を強めに出す
        let running = row
            .running
            .as_ref()
            .map(|r| format!(" [実行中 pid={} {}]", r.pid, r.label()))
            .unwrap_or_default();
        let message = format!(
            "{verb}しますか? {} {}{running}  (y/n)",
            row.short_id(),
            crate::session::truncate_chars(&row.title, 40)
        );
        self.mode = Mode::Confirm { kind, message };
        Effect::None
    }

    fn on_key_confirm(&mut self, key: KeyEvent, kind: ConfirmKind) -> Effect {
        match key.code {
            KeyCode::Char('y') | KeyCode::Char('Y') => {
                self.mode = Mode::Normal;
                match self.selected_index() {
                    Some(i) => match kind {
                        ConfirmKind::Delete => Effect::Delete(i),
                        ConfirmKind::Archive => Effect::Archive(i),
                    },
                    None => Effect::None,
                }
            }
            _ => {
                self.mode = Mode::Normal;
                self.set_status("中止した");
                Effect::None
            }
        }
    }

    fn on_key_input(
        &mut self,
        key: KeyEvent,
        kind: InputKind,
        prompt: String,
        mut buffer: String,
    ) -> Effect {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        if ctrl && key.code == KeyCode::Char('u') {
            self.mode = Mode::Input {
                kind,
                prompt,
                buffer: String::new(),
            };
            return Effect::None;
        }
        if ctrl && key.code == KeyCode::Char('c') {
            self.mode = Mode::Normal;
            return Effect::None;
        }

        match key.code {
            KeyCode::Esc => {
                self.mode = Mode::Normal;
                Effect::None
            }
            KeyCode::Enter => {
                self.mode = Mode::Normal;
                let text = buffer.trim().to_string();
                match kind {
                    InputKind::Grep => {
                        if text.is_empty() {
                            self.clear_content_hits();
                            self.set_status("内容検索を解除した");
                            Effect::None
                        } else {
                            Effect::Grep(text)
                        }
                    }
                    InputKind::Tag => match self.selected_index() {
                        Some(i) if text.is_empty() => Effect::ClearTags(i),
                        Some(i) => Effect::AddTag(i, text),
                        None => Effect::None,
                    },
                }
            }
            KeyCode::Backspace => {
                buffer.pop();
                self.mode = Mode::Input { kind, prompt, buffer };
                Effect::None
            }
            KeyCode::Char(c) => {
                buffer.push(c);
                self.mode = Mode::Input { kind, prompt, buffer };
                Effect::None
            }
            _ => Effect::None,
        }
    }
}

#[cfg(test)]
#[allow(non_snake_case)] // テスト名は日本語で書く
mod tests {
    use super::*;
    use crate::registry::RunningSession;
    use crate::rows;
    use crate::scan::{ScanTarget, ScannedSession};
    use crate::session::TitleKind;
    use crate::tasks::TaskSummary;
    use crate::worklog::WorklogSummary;
    use std::collections::HashMap;
    use std::path::PathBuf;
    use std::time::{Duration, SystemTime};

    fn scanned(id: &str, title: &str, secs: u64) -> ScannedSession {
        ScannedSession {
            target: ScanTarget {
                path: PathBuf::from(format!("/p/proj/{id}.jsonl")),
                kind: crate::scan::EntryKind::Session,
                project_dir: "proj".into(),
                file_stem: id.into(),
                size: 10,
                mtime_ns: 0,
                created: Some(SystemTime::UNIX_EPOCH + Duration::from_secs(secs)),
                modified: None,
            },
            session_id: id.into(),
            cwd: Some("/Users/work/.ghq/repo".into()),
            title: title.into(),
            title_kind: TitleKind::Custom,
            first_prompt: None,
            line_count: 1,
        }
    }

    fn app_with_running(running: HashMap<String, RunningSession>) -> App {
        let tasks: HashMap<String, TaskSummary> = HashMap::new();
        let wl: HashMap<String, WorklogSummary> = HashMap::new();
        let tags: HashMap<String, Vec<String>> = HashMap::new();
        let rows = rows::build(
            vec![
                scanned("aaaa1111", "[#55710] WAFボット対策", 300),
                scanned("bbbb2222", "コスト削減PJ", 200),
                scanned("cccc3333", "termmap 開発", 100),
            ],
            &tasks,
            &running,
            &wl,
            &tags,
        );
        App::new(rows)
    }

    fn app() -> App {
        app_with_running(HashMap::new())
    }

    fn key(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)
    }
    fn ctrl(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
    }
    fn code(c: KeyCode) -> KeyEvent {
        KeyEvent::new(c, KeyModifiers::NONE)
    }

    #[test]
    fn 初期状態は全件表示で先頭選択() {
        let app = app();
        assert_eq!(app.counts(), (3, 3));
        assert_eq!(app.cursor, 0);
        assert_eq!(app.selected().unwrap().session_id, "aaaa1111");
        assert_eq!(app.mode, Mode::Normal);
    }

    #[test]
    fn 文字入力で絞り込む() {
        let mut app = app();
        assert_eq!(app.on_key(key('コ')), Effect::None);
        assert_eq!(app.query, "コ");
        assert_eq!(app.counts().0, 1);
        assert_eq!(app.selected().unwrap().session_id, "bbbb2222");
    }

    #[test]
    fn バックスペースで戻せる() {
        let mut app = app();
        app.on_key(key('コ'));
        app.on_key(key('ス'));
        assert_eq!(app.counts().0, 1);
        app.on_key(code(KeyCode::Backspace));
        app.on_key(code(KeyCode::Backspace));
        assert_eq!(app.query, "");
        assert_eq!(app.counts().0, 3);
    }

    #[test]
    fn ctrl_uでクエリを消す() {
        let mut app = app();
        app.on_key(key('W'));
        app.on_key(ctrl('u'));
        assert_eq!(app.query, "");
        assert_eq!(app.counts().0, 3);
    }

    #[test]
    fn カーソル移動は端で止まる() {
        let mut app = app();
        app.on_key(code(KeyCode::Up));
        assert_eq!(app.cursor, 0);
        app.on_key(code(KeyCode::Down));
        assert_eq!(app.cursor, 1);
        app.on_key(ctrl('n'));
        assert_eq!(app.cursor, 2);
        app.on_key(ctrl('n'));
        assert_eq!(app.cursor, 2);
        app.on_key(ctrl('p'));
        assert_eq!(app.cursor, 1);
    }

    #[test]
    fn ページ移動とホームエンド() {
        let mut app = app();
        app.on_key(code(KeyCode::PageDown));
        assert_eq!(app.cursor, 2);
        app.on_key(code(KeyCode::PageUp));
        assert_eq!(app.cursor, 0);
        app.on_key(code(KeyCode::End));
        assert_eq!(app.cursor, 2);
        app.on_key(code(KeyCode::Home));
        assert_eq!(app.cursor, 0);
    }

    #[test]
    fn 絞り込むとカーソルは先頭に戻る() {
        let mut app = app();
        app.on_key(code(KeyCode::Down));
        app.on_key(code(KeyCode::Down));
        assert_eq!(app.cursor, 2);
        app.on_key(key('コ'));
        assert_eq!(app.cursor, 0);
    }

    #[test]
    fn 該当なしでも落ちない() {
        let mut app = app();
        for c in "zzzzz存在しない".chars() {
            app.on_key(key(c));
        }
        assert_eq!(app.counts().0, 0);
        assert!(app.selected().is_none());
        assert_eq!(app.on_key(code(KeyCode::Enter)), Effect::None);
        assert_eq!(app.on_key(ctrl('d')), Effect::None);
        assert_eq!(app.on_key(ctrl('r')), Effect::None);
        app.on_key(code(KeyCode::Down));
        assert_eq!(app.cursor, 0);
    }

    #[test]
    fn EnterでresumeのEffectが出る() {
        let mut app = app();
        app.on_key(code(KeyCode::Down));
        assert_eq!(app.on_key(code(KeyCode::Enter)), Effect::Resume(1));
    }

    #[test]
    fn ctrl_cで終了() {
        let mut app = app();
        assert_eq!(app.on_key(ctrl('c')), Effect::Quit);
    }

    #[test]
    fn escは段階的に解除して最後に終了する() {
        let mut app = app();
        // クエリがあればまずクエリを消す
        app.on_key(key('コ'));
        assert_eq!(app.on_key(code(KeyCode::Esc)), Effect::None);
        assert_eq!(app.query, "");
        // 内容検索が効いていれば次はそれを解除
        app.set_content_hits("x".into(), HashSet::from(["aaaa1111".to_string()]));
        assert_eq!(app.on_key(code(KeyCode::Esc)), Effect::None);
        assert!(app.content_hits.is_none());
        // 何も無ければ終了
        assert_eq!(app.on_key(code(KeyCode::Esc)), Effect::Quit);
    }

    #[test]
    fn 削除は確認を挟む() {
        let mut app = app();
        assert_eq!(app.on_key(ctrl('d')), Effect::None);
        match &app.mode {
            Mode::Confirm { kind, message } => {
                assert_eq!(*kind, ConfirmKind::Delete);
                assert!(message.contains("削除しますか?"));
                assert!(message.contains("aaaa1111"));
            }
            other => panic!("確認モードになっていない: {other:?}"),
        }
        assert_eq!(app.on_key(key('y')), Effect::Delete(0));
        assert_eq!(app.mode, Mode::Normal);
    }

    #[test]
    fn 確認をnで中止できる() {
        let mut app = app();
        app.on_key(ctrl('d'));
        assert_eq!(app.on_key(key('n')), Effect::None);
        assert_eq!(app.mode, Mode::Normal);
        assert_eq!(app.status.as_deref(), Some("中止した"));
        // 行は減っていない
        assert_eq!(app.counts(), (3, 3));
    }

    #[test]
    fn 確認はescでも中止できる() {
        let mut app = app();
        app.on_key(ctrl('a'));
        assert_eq!(app.on_key(code(KeyCode::Esc)), Effect::None);
        assert_eq!(app.mode, Mode::Normal);
    }

    #[test]
    fn 実行中セッションは確認文言に警告が出る() {
        let mut running = HashMap::new();
        running.insert(
            "aaaa1111".to_string(),
            RunningSession {
                pid: 12063,
                session_id: "aaaa1111".into(),
                cwd: None,
                name: None,
                status: Some("busy".into()),
                kind: Some("interactive".into()),
            },
        );
        let mut app = app_with_running(running);
        app.on_key(ctrl('d'));
        match &app.mode {
            Mode::Confirm { message, .. } => {
                assert!(message.contains("実行中"), "{message}");
                assert!(message.contains("12063"), "{message}");
            }
            other => panic!("確認モードになっていない: {other:?}"),
        }
    }

    #[test]
    fn アーカイブも確認を挟む() {
        let mut app = app();
        app.on_key(ctrl('a'));
        assert!(matches!(
            app.mode,
            Mode::Confirm { kind: ConfirmKind::Archive, .. }
        ));
        assert_eq!(app.on_key(key('y')), Effect::Archive(0));
    }

    #[test]
    fn 行を消すと一覧から消える() {
        let mut app = app();
        app.remove_row(0);
        assert_eq!(app.counts(), (2, 2));
        assert_eq!(app.selected().unwrap().session_id, "bbbb2222");
    }

    #[test]
    fn 末尾を消したらカーソルが繰り上がる() {
        let mut app = app();
        app.cursor = 2;
        app.remove_row(2);
        assert_eq!(app.cursor, 1);
        assert_eq!(app.selected().unwrap().session_id, "bbbb2222");
    }

    #[test]
    fn 内容検索の入力からEffectが出る() {
        let mut app = app();
        app.on_key(ctrl('g'));
        assert!(matches!(app.mode, Mode::Input { kind: InputKind::Grep, .. }));
        for c in "CrateDB".chars() {
            app.on_key(key(c));
        }
        assert_eq!(app.on_key(code(KeyCode::Enter)), Effect::Grep("CrateDB".into()));
        assert_eq!(app.mode, Mode::Normal);
    }

    #[test]
    fn 内容検索を空Enterで解除する() {
        let mut app = app();
        app.set_content_hits("x".into(), HashSet::from(["aaaa1111".to_string()]));
        assert_eq!(app.counts().0, 1);
        app.on_key(ctrl('g'));
        assert_eq!(app.on_key(code(KeyCode::Enter)), Effect::None);
        assert_eq!(app.counts().0, 3);
    }

    #[test]
    fn 内容検索の入力はescで中止できる() {
        let mut app = app();
        app.on_key(ctrl('g'));
        app.on_key(key('a'));
        assert_eq!(app.on_key(code(KeyCode::Esc)), Effect::None);
        assert_eq!(app.mode, Mode::Normal);
        assert!(app.content_hits.is_none());
    }

    #[test]
    fn 内容検索結果とクエリが両方効く() {
        let mut app = app();
        app.set_content_hits(
            "kw".into(),
            HashSet::from(["bbbb2222".to_string(), "cccc3333".to_string()]),
        );
        assert_eq!(app.counts().0, 2);
        app.on_key(key('t'));
        app.on_key(key('e'));
        assert_eq!(app.counts().0, 1);
        assert_eq!(app.selected().unwrap().session_id, "cccc3333");
    }

    #[test]
    fn タグ入力からEffectが出る() {
        let mut app = app();
        app.on_key(ctrl('t'));
        assert!(matches!(app.mode, Mode::Input { kind: InputKind::Tag, .. }));
        for c in "重要".chars() {
            app.on_key(key(c));
        }
        assert_eq!(app.on_key(code(KeyCode::Enter)), Effect::AddTag(0, "重要".into()));
    }

    #[test]
    fn タグを空Enterで全解除する() {
        let mut app = app();
        app.on_key(ctrl('t'));
        assert_eq!(app.on_key(code(KeyCode::Enter)), Effect::ClearTags(0));
    }

    #[test]
    fn 入力中のctrl_uでバッファを消す() {
        let mut app = app();
        app.on_key(ctrl('g'));
        for c in "abc".chars() {
            app.on_key(key(c));
        }
        app.on_key(ctrl('u'));
        match &app.mode {
            Mode::Input { buffer, .. } => assert_eq!(buffer, ""),
            other => panic!("入力モードのままのはず: {other:?}"),
        }
    }

    #[test]
    fn 入力中のバックスペース() {
        let mut app = app();
        app.on_key(ctrl('g'));
        app.on_key(key('a'));
        app.on_key(key('b'));
        app.on_key(code(KeyCode::Backspace));
        match &app.mode {
            Mode::Input { buffer, .. } => assert_eq!(buffer, "a"),
            other => panic!("入力モードのままのはず: {other:?}"),
        }
    }

    #[test]
    fn タグ反映で一覧の表示が変わる() {
        let mut app = app();
        app.set_tags(0, vec!["重要".into()]);
        assert_eq!(app.rows[0].format_tags(), "重要");
        // タグは fuzzy 検索の対象にも入る
        app.query = "重要".into();
        app.refilter();
        assert_eq!(app.counts().0, 1);
    }

    #[test]
    fn recapのEffectが出る() {
        let mut app = app();
        app.on_key(code(KeyCode::Down));
        assert_eq!(app.on_key(ctrl('r')), Effect::Recap(1));
    }

    #[test]
    fn ctrl_sでサブエージェント切替のEffectが出る() {
        let mut app = app();
        assert_eq!(app.on_key(ctrl('s')), Effect::ToggleSubagents);
        // モードは変わらない (即座に切替を外側へ委ねるだけ)
        assert_eq!(app.mode, Mode::Normal);
    }

    #[test]
    fn サブエージェント切替は選択が無くても出る() {
        // 削除・アーカイブ等と違い、一覧全体に効く操作なので選択行が無くても動く
        let mut app = App::new(Vec::new());
        assert_eq!(app.on_key(ctrl('s')), Effect::ToggleSubagents);
    }

    #[test]
    fn 既定ではサブエージェント記録は非表示扱い() {
        assert!(!app().include_subagents);
    }

    #[test]
    fn 行の入れ替えで同じセッションにカーソルが追従する() {
        let mut app = app();
        app.on_key(code(KeyCode::Down)); // bbbb2222 を選択
        assert_eq!(app.selected().unwrap().session_id, "bbbb2222");

        // 新しい行 (サブエージェント記録を含めた再走査結果) を先頭に割り込ませる
        let tasks: HashMap<String, TaskSummary> = HashMap::new();
        let running: HashMap<String, RunningSession> = HashMap::new();
        let wl: HashMap<String, WorklogSummary> = HashMap::new();
        let tags: HashMap<String, Vec<String>> = HashMap::new();
        let new_rows = rows::build(
            vec![
                scanned("zzzz9999", "割り込んだ行", 400),
                scanned("aaaa1111", "WAFボット対策", 300),
                scanned("bbbb2222", "コスト削減PJ", 200),
                scanned("cccc3333", "termmap 開発", 100),
            ],
            &tasks,
            &running,
            &wl,
            &tags,
        );
        app.replace_rows(new_rows);

        assert_eq!(app.counts(), (4, 4));
        assert_eq!(app.selected().unwrap().session_id, "bbbb2222");
    }

    #[test]
    fn 選択中セッションが消えたらクランプ位置のまま() {
        let mut app = app();
        app.cursor = 2; // cccc3333 を選択
        assert_eq!(app.selected().unwrap().session_id, "cccc3333");

        let tasks: HashMap<String, TaskSummary> = HashMap::new();
        let running: HashMap<String, RunningSession> = HashMap::new();
        let wl: HashMap<String, WorklogSummary> = HashMap::new();
        let tags: HashMap<String, Vec<String>> = HashMap::new();
        // cccc3333 が居なくなった新しい一覧に入れ替える
        let new_rows = rows::build(
            vec![scanned("aaaa1111", "WAFボット対策", 300), scanned("bbbb2222", "コスト削減PJ", 200)],
            &tasks,
            &running,
            &wl,
            &tags,
        );
        app.replace_rows(new_rows);

        // 落ちずに範囲内へクランプされる
        assert_eq!(app.counts(), (2, 2));
        assert!(app.selected().is_some());
    }

    #[test]
    fn 空一覧への入れ替えでも落ちない() {
        let mut app = app();
        app.replace_rows(Vec::new());
        assert_eq!(app.counts(), (0, 0));
        assert!(app.selected().is_none());
    }

    #[test]
    fn 空一覧でも操作して落ちない() {
        let mut app = App::new(Vec::new());
        assert_eq!(app.counts(), (0, 0));
        assert!(app.selected().is_none());
        assert_eq!(app.on_key(code(KeyCode::Enter)), Effect::None);
        assert_eq!(app.on_key(ctrl('t')), Effect::None);
        assert_eq!(app.on_key(ctrl('a')), Effect::None);
        assert_eq!(app.on_key(code(KeyCode::Esc)), Effect::Quit);
    }

    #[test]
    fn 範囲外のremove_rowは無視する() {
        let mut app = app();
        app.remove_row(99);
        assert_eq!(app.counts(), (3, 3));
    }
}
