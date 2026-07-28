//! 描画のみ。表示用の文字列組み立ては [`row_cells`] などに切り出してテストできるようにしてある。

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Cell, Clear, Paragraph, Row, Table, TableState};

use crate::rows::SessionRow;
use crate::session::oneline_preview;
use crate::tui::app::{App, Mode};

/// 一覧の列見出し。
pub const HEADERS: [&str; 6] = ["ID", "作成日時", "P/I/D", "チケット", "タイトル", "プロジェクト"];

/// 実行中セッションに付ける印。
const RUNNING_MARK: &str = "●";

/// cwd が消えている (resume 不可) セッションに付ける印 (機能6)。
const CWD_MISSING_MARK: &str = "✗";

/// 1 行分のセル文字列を作る。
pub fn row_cells(row: &SessionRow) -> [String; 6] {
    let id = if row.is_running() {
        format!("{RUNNING_MARK}{}", row.short_id())
    } else {
        format!(" {}", row.short_id())
    };
    let mut title = row.title.clone();
    if !row.tags.is_empty() {
        title = format!("{title} [{}]", row.format_tags());
    }
    // cwd が消えている行は resume できないので、プロジェクト欄に印を付ける (機能6)
    let project = if row.cwd_missing() {
        format!("{CWD_MISSING_MARK} {}", row.project_label())
    } else {
        row.project_label()
    };
    [
        id,
        row.format_created(),
        row.format_tasks(),
        row.format_tickets(),
        title,
        project,
    ]
}

/// 選択行の詳細 (画面下部)。
pub fn detail_lines(row: &SessionRow) -> Vec<String> {
    let mut out = Vec::new();
    // cwd が消えていれば resume できない旨を明示する (機能6)
    let cwd = match &row.cwd {
        None => "(cwd 不明)".to_string(),
        Some(c) if row.cwd_missing() => format!("{c}  ({CWD_MISSING_MARK} 消滅・resume 不可)"),
        Some(c) => c.clone(),
    };
    out.push(format!("{}  {}", row.session_id, cwd));

    let mut meta = vec![
        format!("{} 行", row.line_count),
        crate::rows::format_size(row.size),
    ];
    if !row.worklog.format_elapsed().is_empty() {
        meta.push(format!("作業 {}", row.worklog.format_elapsed()));
    }
    if let Some(r) = &row.running {
        meta.push(format!("実行中 pid={} {}", r.pid, r.label()));
    }
    let modified = row.format_modified();
    if !modified.is_empty() {
        meta.push(format!("更新 {modified}"));
    }
    out.push(meta.join("  "));

    if let Some(p) = &row.first_prompt {
        out.push(format!("> {}", oneline_preview(p, 200)));
    }
    out
}

/// サブエージェント記録の表示状態を表す文言。
fn subagent_state_label(app: &App) -> &'static str {
    if app.include_subagents { "表示中" } else { "非表示" }
}

/// ヘッダ行 (クエリと件数)。サブエージェント記録の表示状態は常に出す。
pub fn header_line(app: &App) -> String {
    let (shown, total) = app.counts();
    let mut s = format!("> {}", app.query);
    if let Some(g) = &app.grep_query {
        s.push_str(&format!("   [内容検索: {g}]"));
    }
    s.push_str(&format!("   [サブエージェント:{}]", subagent_state_label(app)));
    s.push_str(&format!("   {shown}/{total}"));
    s
}

/// ステータス行に常時出す一行ヘルプ。詳しい一覧は `?` のオーバーレイ (機能5)。
pub const HELP: &str = "Enter:resume  ^w:cwd指定resume  ^g:内容検索  ^t:タグ  ^d:削除  ^a:アーカイブ  ?:ヘルプ  Esc:戻る/終了";

/// `?` で開くヘルプオーバーレイの中身 (機能5)。1 行 1 ショートカット。
pub const HELP_LINES: [&str; 17] = [
    "cst セッションブラウザ ― ショートカット",
    "",
    "  Enter        選択セッションを resume (cwd が消えていると不可)",
    "  ^w           cwd を一時指定して resume (jsonl は書き換えない)",
    "  ↑/↓ ^p/^n    カーソル移動      PgUp/PgDn 10 行  Home/End 端へ",
    "  文字入力      タイトル/セッションID/cwd/チケット/タグを fuzzy 絞り込み",
    "  貼り付け      セッションID等をそのまま貼り付け可 (クエリ/入力欄どちらも)",
    "  入力欄        ←→ で移動  Home/End 端へ  Del 削除  ^a/^e 行頭/行末",
    "  ^u           クエリ/入力をクリア",
    "  ^g           jsonl 全文検索 (内容検索)",
    "  ^t           タグ付け (空 Enter で全解除)",
    "  ^r           セッションを要約 (recap)",
    "  ^s           サブエージェント記録の表示切替",
    "  ^d / ^a      削除 / アーカイブ (確認あり)",
    "  ?            このヘルプ",
    "  Esc          クエリ→内容検索→終了 の順に解除    ^c 即終了",
    "  任意のキーで閉じる",
];

/// 画面全体を描く。
pub fn draw(frame: &mut Frame, app: &App, state: &mut TableState) {
    let detail_height = app.selected().map(|r| detail_lines(r).len() as u16).unwrap_or(1) + 2;
    let areas = Layout::vertical([
        Constraint::Length(1),             // クエリ
        Constraint::Min(3),                // 一覧
        Constraint::Length(detail_height), // 詳細
        Constraint::Length(1),             // ステータス / ヘルプ
    ])
    .split(frame.area());

    draw_query(frame, areas[0], app);
    draw_table(frame, areas[1], app, state);
    draw_detail(frame, areas[2], app);
    let cursor = draw_status(frame, areas[3], app);

    // ヘルプは一覧の上に重ねて出す (機能5)。
    // 入力中 (^w の cwd 指定など) はテキストカーソルを末尾に出す。
    // ヘルプと入力は同時に立たないので分岐でよい。
    if app.mode == Mode::Help {
        draw_help(frame);
    } else if let Some((x, y)) = cursor {
        frame.set_cursor_position((x, y));
    }
}

/// ヘルプオーバーレイを画面中央に重ねて描く (機能5)。
fn draw_help(frame: &mut Frame) {
    let area = centered_rect(frame.area(), &HELP_LINES);
    let text: Vec<Line> = HELP_LINES.iter().map(|l| Line::from(*l)).collect();
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" ヘルプ (任意のキーで閉じる) ")
        .style(Style::default().fg(Color::White).bg(Color::Black));
    // 下地を消してから重ねる
    frame.render_widget(Clear, area);
    frame.render_widget(Paragraph::new(text).block(block), area);
}

/// ヘルプ本文が収まる矩形を画面中央に作る。画面が小さければ全面に丸める。
fn centered_rect(full: Rect, lines: &[&str]) -> Rect {
    // 罫線 + 左右の余白を見込んだ幅・高さ
    let content_w = lines.iter().map(|l| l.chars().count()).max().unwrap_or(0) as u16;
    let want_w = (content_w + 4).min(full.width);
    let want_h = (lines.len() as u16 + 2).min(full.height);
    let x = full.x + (full.width.saturating_sub(want_w)) / 2;
    let y = full.y + (full.height.saturating_sub(want_h)) / 2;
    Rect {
        x,
        y,
        width: want_w,
        height: want_h,
    }
}

fn draw_query(frame: &mut Frame, area: Rect, app: &App) {
    let line = Line::from(vec![Span::styled(
        header_line(app),
        Style::default().add_modifier(Modifier::BOLD),
    )]);
    frame.render_widget(Paragraph::new(line), area);
}

fn draw_table(frame: &mut Frame, area: Rect, app: &App, state: &mut TableState) {
    let header = Row::new(HEADERS.iter().map(|h| Cell::from(*h)))
        .style(Style::default().add_modifier(Modifier::BOLD).fg(Color::Cyan));

    let rows: Vec<Row> = app
        .filtered
        .iter()
        .map(|&i| {
            let row = &app.rows[i];
            let cells = row_cells(row);
            // 実行中は緑、cwd が消えた resume 不可の行は暗く (機能6)。実行中を優先。
            let style = if row.is_running() {
                Style::default().fg(Color::Green)
            } else if row.cwd_missing() {
                Style::default().fg(Color::DarkGray)
            } else {
                Style::default()
            };
            Row::new(cells.into_iter().map(Cell::from)).style(style)
        })
        .collect();

    let widths = [
        Constraint::Length(9),
        Constraint::Length(16),
        Constraint::Length(8),
        Constraint::Length(16),
        Constraint::Min(20),
        Constraint::Length(22),
    ];

    let table = Table::new(rows, widths)
        .header(header)
        .row_highlight_style(Style::default().add_modifier(Modifier::REVERSED))
        .highlight_symbol("");

    state.select(if app.filtered.is_empty() {
        None
    } else {
        Some(app.cursor)
    });
    frame.render_stateful_widget(table, area, state);
}

fn draw_detail(frame: &mut Frame, area: Rect, app: &App) {
    let text: Vec<Line> = match app.selected() {
        Some(row) => detail_lines(row).into_iter().map(Line::from).collect(),
        None => vec![Line::from("該当なし")],
    };
    frame.render_widget(
        Paragraph::new(text).block(Block::default().borders(Borders::TOP)),
        area,
    );
}

/// 文字列の端末表示幅 (全角=2)。ratatui の `Line::width()` と同じ算出。
fn display_width(s: &str) -> usize {
    Line::from(s).width()
}

/// 入力欄の見え方を決める。返り値は (実際に描く文字列, カーソルの桁位置)。
///
/// `cursor` は buffer 内のカーソル文字位置 (0..=文字数)。
/// `prompt: ` は常に左端に固定し、buffer 部分だけを横スクロールさせて
/// カーソルが可視域に入るようにする。全角は 2 桁として数える。
fn input_view(prompt: &str, buffer: &str, cursor: usize, width: usize) -> (String, usize) {
    let head = format!("{prompt}: ");
    let head_w = display_width(&head);
    let chars: Vec<char> = buffer.chars().collect();

    // buffer 部分に割ける桁数。prompt で使い切っていたら最低 1 桁は残す。
    let avail = width.saturating_sub(head_w).max(1);

    // カーソルまでの buffer 表示幅 (先頭からカーソル位置まで)。
    let cursor_w = display_width(&chars[..cursor.min(chars.len())].iter().collect::<String>());
    let total_w = display_width(buffer);

    // 全部収まるならスクロールしない。
    if width == 0 || head_w + total_w < width {
        return (format!("{head}{buffer}"), head_w + cursor_w);
    }

    // カーソルが可視域 (avail-1 桁ぶん) に収まるよう、buffer の表示開始桁 off を決める。
    // カーソルは右端の 1 つ内側までに収める。
    let visible = avail.saturating_sub(1).max(1);
    let off = cursor_w.saturating_sub(visible);

    // off 桁ぶん左を捨てた buffer を作る (文字境界・全角を尊重)。
    let mut skipped = 0;
    let mut start = 0;
    for (i, c) in chars.iter().enumerate() {
        if skipped >= off {
            start = i;
            break;
        }
        skipped += display_width(&c.to_string());
        start = i + 1;
    }
    // 右端は avail 桁で切る。
    let mut shown = String::new();
    let mut w = 0;
    for c in &chars[start..] {
        let cw = display_width(&c.to_string());
        if w + cw > avail {
            break;
        }
        shown.push(*c);
        w += cw;
    }
    let cursor_col = head_w + cursor_w.saturating_sub(skipped);
    (format!("{head}{shown}"), cursor_col)
}

/// ステータス行を描く。入力中はカーソルの絶対座標 (col,row) を返す。
fn draw_status(frame: &mut Frame, area: Rect, app: &App) -> Option<(u16, u16)> {
    let (text, style, cursor) = match &app.mode {
        Mode::Confirm { message, .. } => (
            message.clone(),
            Style::default().fg(Color::Black).bg(Color::Yellow),
            None,
        ),
        Mode::Input { prompt, input, .. } => {
            let (shown, col) = input_view(prompt, &input.buffer, input.cursor, area.width as usize);
            let x = area.x + col.min(area.width.saturating_sub(1) as usize) as u16;
            (
                shown,
                Style::default().fg(Color::Black).bg(Color::Cyan),
                Some((x, area.y)),
            )
        }
        Mode::Normal => match &app.status {
            Some(s) => (s.clone(), Style::default().fg(Color::Yellow), None),
            None => (HELP.to_string(), Style::default().fg(Color::DarkGray), None),
        },
        // ヘルプ表示中はステータス行にも案内を出す
        Mode::Help => (
            "任意のキーで閉じる".to_string(),
            Style::default().fg(Color::DarkGray),
            None,
        ),
    };
    frame.render_widget(Paragraph::new(Line::from(text)).style(style), area);
    cursor
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

    fn build_row(running: bool, tags: Vec<String>) -> SessionRow {
        let created = Some(SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000));
        let scanned = ScannedSession {
            target: ScanTarget {
                path: PathBuf::from("/p/proj/aaaa1111-2222.jsonl"),
                kind: crate::scan::EntryKind::Session,
                project_dir: "-Users-work--ghq-ss-es-teppai".into(),
                file_stem: "aaaa1111-2222".into(),
                size: 2048,
                mtime_ns: 0,
                created,
                modified: Some(SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_003_600)),
            },
            session_id: "aaaa1111-2222".into(),
            cwd: Some("/Users/work/.ghq/ss_es_teppai".into()),
            title: "[#55710] WAFボット対策".into(),
            title_kind: TitleKind::Custom,
            first_prompt: Some("最初の\n質問".into()),
            line_count: 120,
            created,
        };

        let mut tasks = HashMap::new();
        tasks.insert(
            "aaaa1111-2222".to_string(),
            TaskSummary {
                pending: 2,
                in_progress: 1,
                completed: 5,
                other: 0,
                subjects: vec![],
            },
        );
        let mut run_map = HashMap::new();
        if running {
            run_map.insert(
                "aaaa1111-2222".to_string(),
                RunningSession {
                    pid: 999,
                    session_id: "aaaa1111-2222".into(),
                    cwd: None,
                    name: None,
                    status: Some("idle".into()),
                    kind: Some("bg".into()),
                },
            );
        }
        let mut wl = HashMap::new();
        wl.insert(
            "aaaa1111-2222".to_string(),
            WorklogSummary {
                elapsed_sec: 5000,
                blocks: 3,
                tickets: vec![],
            },
        );
        let mut tag_map = HashMap::new();
        if !tags.is_empty() {
            tag_map.insert("aaaa1111-2222".to_string(), tags);
        }

        let mut row = rows::build(vec![scanned], &tasks, &run_map, &wl, &tag_map)
            .into_iter()
            .next()
            .unwrap();
        // cwd の実在はマシン依存なので、表示テストが安定するよう既定で「現存」に固定する。
        // 消滅時の表示は専用テストで cwd_exists=false を明示して確かめる。
        row.cwd_exists = true;
        row
    }

    #[test]
    fn セルの内容() {
        let row = build_row(false, vec![]);
        let cells = row_cells(&row);
        assert_eq!(cells[0], " aaaa1111");
        assert_eq!(cells[1].len(), 16);
        assert_eq!(cells[2], "2/1/5");
        assert_eq!(cells[3], "#55710");
        assert_eq!(cells[4], "[#55710] WAFボット対策");
        assert_eq!(cells[5], "ss_es_teppai");
    }

    #[test]
    fn 実行中は印がつく() {
        let cells = row_cells(&build_row(true, vec![]));
        assert!(cells[0].starts_with(RUNNING_MARK));
    }

    #[test]
    fn cwdが現存すればプロジェクト欄に印は付かない() {
        let cells = row_cells(&build_row(false, vec![]));
        assert!(!cells[5].starts_with(CWD_MISSING_MARK));
    }

    #[test]
    fn cwdが消えていればプロジェクト欄に印がつく() {
        let mut row = build_row(false, vec![]);
        row.cwd_exists = false;
        let cells = row_cells(&row);
        assert!(cells[5].starts_with(CWD_MISSING_MARK));
    }

    #[test]
    fn cwd消滅は詳細にresume不可と出る() {
        let mut row = build_row(false, vec![]);
        row.cwd_exists = false;
        let lines = detail_lines(&row);
        assert!(lines[0].contains("resume 不可"));
    }

    #[test]
    fn タグはタイトルの後ろに出る() {
        let cells = row_cells(&build_row(false, vec!["重要".into(), "WAF".into()]));
        assert_eq!(cells[4], "[#55710] WAFボット対策 [重要,WAF]");
    }

    #[test]
    fn 列見出しとセル数が一致する() {
        let cells = row_cells(&build_row(false, vec![]));
        assert_eq!(HEADERS.len(), cells.len());
    }

    #[test]
    fn 詳細行の中身() {
        let row = build_row(true, vec![]);
        let lines = detail_lines(&row);
        assert!(lines[0].contains("aaaa1111-2222"));
        assert!(lines[0].contains("/Users/work/.ghq/ss_es_teppai"));
        assert!(lines[1].contains("120 行"));
        assert!(lines[1].contains("2.0K"));
        assert!(lines[1].contains("作業 1h23m"));
        assert!(lines[1].contains("実行中 pid=999"));
        // 改行はスペースに畳む
        assert_eq!(lines[2], "> 最初の 質問");
    }

    #[test]
    fn cwd不明でも詳細を出せる() {
        let mut row = build_row(false, vec![]);
        row.cwd = None;
        row.first_prompt = None;
        let lines = detail_lines(&row);
        assert!(lines[0].contains("(cwd 不明)"));
        assert_eq!(lines.len(), 2);
    }

    #[test]
    fn 詳細行のプレビューはthinkingマーカーを取り除く() {
        let mut row = build_row(false, vec![]);
        row.first_prompt = Some("[thinking...]本文だけ表示したい".into());
        let lines = detail_lines(&row);
        assert_eq!(lines[2], "> 本文だけ表示したい");
    }

    #[test]
    fn ヘッダ行に件数とクエリが出る() {
        let app = App::new(vec![build_row(false, vec![])]);
        let line = header_line(&app);
        assert!(line.starts_with("> "));
        assert!(line.contains("1/1"));
        assert!(!line.contains("内容検索"));
    }

    #[test]
    fn 内容検索中はヘッダに出る() {
        let mut app = App::new(vec![build_row(false, vec![])]);
        app.set_content_hits("CrateDB".into(), Default::default());
        assert!(header_line(&app).contains("[内容検索: CrateDB]"));
    }

    #[test]
    fn ヘッダにサブエージェント記録の表示状態が出る() {
        let mut app = App::new(vec![build_row(false, vec![])]);
        assert!(header_line(&app).contains("[サブエージェント:非表示]"));

        app.include_subagents = true;
        assert!(header_line(&app).contains("[サブエージェント:表示中]"));
    }

    #[test]
    fn 一行ヘルプに主要キーの案内がある() {
        assert!(HELP.contains("?:ヘルプ"));
        assert!(HELP.contains("^w"));
    }

    #[test]
    fn ヘルプオーバーレイにサブエージェント切替の案内がある() {
        // 詳しい案内はオーバーレイ側に移した
        assert!(HELP_LINES.iter().any(|l| l.contains("^s")));
        assert!(HELP_LINES.iter().any(|l| l.contains("^w")));
    }

    #[test]
    fn ヘルプ表示中はオーバーレイが描かれる() {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;

        let mut app = App::new(vec![build_row(false, vec![])]);
        app.mode = Mode::Help;
        let mut terminal = Terminal::new(TestBackend::new(120, 24)).unwrap();
        let mut state = TableState::default();
        terminal.draw(|f| draw(f, &app, &mut state)).unwrap();
        let dump = terminal.backend().to_string();
        assert!(dump.contains("ショートカット"));
        assert!(dump.contains("cwd を一時指定"));
    }

    #[test]
    fn 小さい画面でもヘルプで落ちない() {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;

        let mut app = App::new(vec![build_row(false, vec![])]);
        app.mode = Mode::Help;
        let mut terminal = Terminal::new(TestBackend::new(20, 6)).unwrap();
        let mut state = TableState::default();
        terminal.draw(|f| draw(f, &app, &mut state)).unwrap();
    }

    #[test]
    fn 画面全体を描いても落ちない() {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;

        let mut app = App::new(vec![build_row(true, vec!["重要".into()])]);
        let mut terminal = Terminal::new(TestBackend::new(120, 20)).unwrap();
        let mut state = TableState::default();

        terminal.draw(|f| draw(f, &app, &mut state)).unwrap();
        let dump = terminal.backend().to_string();
        assert!(dump.contains("aaaa1111"));
        assert!(dump.contains("WAF"));

        // 確認モード・入力モード・空一覧でも描ける
        app.set_status("テスト状態");
        terminal.draw(|f| draw(f, &app, &mut state)).unwrap();

        let mut empty = App::new(Vec::new());
        empty.set_status("該当なし");
        terminal.draw(|f| draw(f, &empty, &mut state)).unwrap();
        assert!(terminal.backend().to_string().contains("該当なし"));
    }

    #[test]
    fn 狭い画面でも描ける() {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;

        let app = App::new(vec![build_row(false, vec![])]);
        let mut terminal = Terminal::new(TestBackend::new(40, 8)).unwrap();
        let mut state = TableState::default();
        terminal.draw(|f| draw(f, &app, &mut state)).unwrap();
    }

    #[test]
    fn 全角の表示幅を数える() {
        assert_eq!(display_width("abc"), 3);
        assert_eq!(display_width("あいう"), 6);
        assert_eq!(display_width("cwd あ"), 6);
    }

    #[test]
    fn 入力が収まればそのまま出しカーソルは末尾() {
        // カーソルは末尾 (4 文字目の後ろ)
        let (shown, col) = input_view("cwd", "/tmp", 4, 80);
        assert_eq!(shown, "cwd: /tmp");
        // "cwd: /tmp" は 9 桁。カーソルはその次 (末尾入力位置)
        assert_eq!(col, 9);
    }

    #[test]
    fn 空入力でもプロンプトの後ろにカーソルが出る() {
        let (shown, col) = input_view("内容検索", "", 0, 80);
        assert_eq!(shown, "内容検索: ");
        // 全角4文字(8) + ": "(2) = 10
        assert_eq!(col, 10);
    }

    #[test]
    fn 途中カーソルはその桁を指す() {
        // "cwd: /tmp" でカーソルを先頭 (buffer index 0) に置く
        let (shown, col) = input_view("cwd", "/tmp", 0, 80);
        assert_eq!(shown, "cwd: /tmp");
        // "cwd: " は 5 桁。buffer 先頭なのでカーソルは 5
        assert_eq!(col, 5);
    }

    #[test]
    fn 長い入力で末尾カーソルなら末尾が見える() {
        // 幅 10 に収まらない長いパス。カーソル末尾なら末尾が見える
        let buf = "/very/long/path/to/dir";
        let (shown, col) = input_view("cwd", buf, buf.chars().count(), 10);
        assert!(display_width(&shown) <= 10);
        assert!(shown.ends_with("dir"), "末尾が見えていない: {shown:?}");
        assert!(col <= 9, "カーソルが右端を越える: {col}");
    }

    #[test]
    fn 長い入力で先頭カーソルなら先頭が見える() {
        // カーソルが先頭にあるときは buffer 先頭が可視域に来る
        let buf = "/very/long/path/to/dir";
        let (shown, col) = input_view("cwd", buf, 0, 10);
        assert!(display_width(&shown) <= 10);
        // "cwd: " の直後 (buffer 先頭) を指す
        assert_eq!(col, display_width("cwd: "));
        assert!(shown.starts_with("cwd: /very"), "先頭が見えていない: {shown:?}");
    }

    #[test]
    fn 全角混じりの長い入力で途中カーソルでも落ちない() {
        // 全角がスクロール境界に絡んでもカーソル桁が underflow しない (saturating_sub の回帰)。
        let buf = "あいうえお/かきくけこ/さしすせそ";
        for cursor in 0..=buf.chars().count() {
            let (shown, col) = input_view("cwd", buf, cursor, 10);
            assert!(display_width(&shown) <= 10, "はみ出した: {shown:?}");
            assert!(col <= 10, "カーソルが可視域を越える: cursor={cursor} col={col}");
        }
    }

    #[test]
    fn 入力モードではテキストカーソルが立つ() {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;
        use ratatui::layout::Rect;
        use crate::tui::app::{InputKind, Mode};
        use crate::tui::input::InputState;

        let mut app = App::new(vec![build_row(false, vec![])]);
        app.mode = Mode::Input {
            kind: InputKind::ResumeCwd,
            prompt: "resume する cwd".to_string(),
            input: InputState::with_text("/tmp"),
        };
        // Frame は Terminal 経由でしか作れないので closure の中で draw_status を検証する。
        let mut terminal = Terminal::new(TestBackend::new(120, 20)).unwrap();
        terminal
            .draw(|f| {
                let area = Rect::new(0, 5, 120, 1);
                let cursor = draw_status(f, area, &app);
                let (x, y) = cursor.expect("入力中はカーソルが出るはず");
                assert_eq!(y, 5, "カーソルはステータス行の y に置かれる");
                // "resume する cwd: /tmp" の末尾。全角4 + "する cwd: /tmp"
                assert!(x > area.x, "カーソルが左端のまま");
            })
            .unwrap();
    }

    #[test]
    fn 通常モードではカーソルを出さない() {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;
        use ratatui::layout::Rect;

        let app = App::new(vec![build_row(false, vec![])]);
        let mut terminal = Terminal::new(TestBackend::new(120, 20)).unwrap();
        terminal
            .draw(|f| {
                let area = Rect::new(0, 5, 120, 1);
                assert!(draw_status(f, area, &app).is_none(), "通常モードでカーソルが出ている");
            })
            .unwrap();
    }
}
