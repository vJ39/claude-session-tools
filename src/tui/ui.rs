//! 描画のみ。表示用の文字列組み立ては [`row_cells`] などに切り出してテストできるようにしてある。

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Cell, Paragraph, Row, Table, TableState};

use crate::rows::SessionRow;
use crate::session::oneline_preview;
use crate::tui::app::{App, Mode};

/// 一覧の列見出し。
pub const HEADERS: [&str; 6] = ["ID", "作成日時", "P/I/D", "チケット", "タイトル", "プロジェクト"];

/// 実行中セッションに付ける印。
const RUNNING_MARK: &str = "●";

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
    [
        id,
        row.format_created(),
        row.format_tasks(),
        row.format_tickets(),
        title,
        row.project_label(),
    ]
}

/// 選択行の詳細 (画面下部)。
pub fn detail_lines(row: &SessionRow) -> Vec<String> {
    let mut out = Vec::new();
    out.push(format!(
        "{}  {}",
        row.session_id,
        row.cwd.as_deref().unwrap_or("(cwd 不明)")
    ));

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

/// キー操作の案内。表示状態そのものはヘッダ行に常時出るので、ここではキーの案内だけ。
pub const HELP: &str = "Enter:resume  ^g:内容検索  ^t:タグ  ^r:要約  ^s:サブエージェント表示切替  ^d:削除  ^a:アーカイブ  ^u:クリア  Esc:戻る/終了";

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
    draw_status(frame, areas[3], app);
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
            let style = if row.is_running() {
                Style::default().fg(Color::Green)
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

fn draw_status(frame: &mut Frame, area: Rect, app: &App) {
    let (text, style) = match &app.mode {
        Mode::Confirm { message, .. } => (
            message.clone(),
            Style::default().fg(Color::Black).bg(Color::Yellow),
        ),
        Mode::Input { prompt, buffer, .. } => (
            format!("{prompt}: {buffer}"),
            Style::default().fg(Color::Black).bg(Color::Cyan),
        ),
        Mode::Normal => match &app.status {
            Some(s) => (s.clone(), Style::default().fg(Color::Yellow)),
            None => (HELP.to_string(), Style::default().fg(Color::DarkGray)),
        },
    };
    frame.render_widget(Paragraph::new(Line::from(text)).style(style), area);
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
        let scanned = ScannedSession {
            target: ScanTarget {
                path: PathBuf::from("/p/proj/aaaa1111-2222.jsonl"),
                kind: crate::scan::EntryKind::Session,
                project_dir: "-Users-work--ghq-ss-es-teppai".into(),
                file_stem: "aaaa1111-2222".into(),
                size: 2048,
                mtime_ns: 0,
                created: Some(SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000)),
                modified: Some(SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_003_600)),
            },
            session_id: "aaaa1111-2222".into(),
            cwd: Some("/Users/work/.ghq/ss_es_teppai".into()),
            title: "[#55710] WAFボット対策".into(),
            title_kind: TitleKind::Custom,
            first_prompt: Some("最初の\n質問".into()),
            line_count: 120,
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

        rows::build(vec![scanned], &tasks, &run_map, &wl, &tag_map)
            .into_iter()
            .next()
            .unwrap()
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
    fn ヘルプにサブエージェント切替キーの案内がある() {
        assert!(HELP.contains("^s"));
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
}
