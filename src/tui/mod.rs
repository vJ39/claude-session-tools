//! TUI セッションブラウザのイベントループ。
//!
//! 状態遷移は [`app`]、描画は [`ui`]、データ読み込みは [`crate::loader`] にある。
//! ここは「イベントを受けて App に渡し、返ってきた [`Effect`] を実行する」だけ。

pub mod app;
pub mod input;
pub mod ui;

use std::time::Duration;

use anyhow::Result;
use ratatui::DefaultTerminal;
use ratatui::crossterm::event::{self, Event, KeyEventKind};
use ratatui::widgets::TableState;

use crate::actions::{self, CommandSpec};
use crate::grep;
use crate::loader;
use crate::paths::Paths;
use crate::scan::ScanOptions;
use crate::store::Store;
use crate::tui::app::{App, Effect};

/// 起動直後に出すサマリー文言。
pub fn status_summary(stats: &crate::scan::ScanStats) -> String {
    let mut s = format!(
        "{} セッション (再走査 {} / キャッシュ {})",
        stats.total, stats.parsed, stats.cached
    );
    if stats.skipped_subagents > 0 {
        s.push_str(&format!(
            " / サブエージェント記録 {} 件は非表示 (--include-subagents で表示)",
            stats.skipped_subagents
        ));
    }
    s
}

/// TUI 終了後にシェル側でやること。
pub enum PostAction {
    /// 何もしない
    None,
    /// resume を実行する
    Resume(CommandSpec),
}

/// TUI を起動する。
pub fn run(paths: &Paths, options: ScanOptions) -> Result<PostAction> {
    paths.ensure_data_dir()?;
    let mut store = Store::open(&paths.store_db())?;

    eprintln!("セッションを走査中...");
    let loaded = loader::load_with(paths, &mut store, options, None)?;
    let stats = loaded.stats;

    let mut app = App::new(loaded.rows);
    app.include_subagents = options.include_subagents;
    app.set_status(status_summary(&stats));

    let mut terminal = ratatui::init();
    let result = event_loop(&mut terminal, &mut app, paths, &mut store, options);
    ratatui::restore();
    result
}

fn event_loop(
    terminal: &mut DefaultTerminal,
    app: &mut App,
    paths: &Paths,
    store: &mut Store,
    mut options: ScanOptions,
) -> Result<PostAction> {
    let mut table_state = TableState::default();

    loop {
        terminal.draw(|f| ui::draw(f, app, &mut table_state))?;

        if !event::poll(Duration::from_millis(250))? {
            continue;
        }
        let key = match event::read()? {
            Event::Key(k) if k.kind == KeyEventKind::Press => k,
            _ => continue,
        };

        match app.on_key(key) {
            Effect::None => {}
            Effect::Quit => return Ok(PostAction::None),
            Effect::Resume(index) => match actions::resume_command(&app.rows[index]) {
                Ok(spec) => return Ok(PostAction::Resume(spec)),
                Err(e) => app.set_status(format!("resume できない: {e}")),
            },
            Effect::ResumeWithCwd(index, cwd) => {
                match actions::resume_command_with_cwd(&app.rows[index], &cwd) {
                    Ok(spec) => return Ok(PostAction::Resume(spec)),
                    Err(e) => app.set_status(format!("resume できない: {e}")),
                }
            }
            Effect::Delete(index) => handle_delete(app, paths, store, index),
            Effect::Archive(index) => handle_archive(app, paths, store, index),
            Effect::Grep(needle) => handle_grep(terminal, app, &mut table_state, needle)?,
            Effect::Recap(index) => handle_recap(terminal, app, index)?,
            Effect::AddTag(index, tag) => handle_add_tag(app, store, index, tag),
            Effect::ClearTags(index) => handle_clear_tags(app, store, index),
            Effect::ToggleSubagents => {
                handle_toggle_subagents(terminal, app, &mut table_state, paths, store, &mut options)?
            }
        }
    }
}

/// 同じ sessionId の jsonl が他の行にも残っているか。
///
/// 実データには同一 sessionId が複数のプロジェクトディレクトリに存在する例がある。
/// その場合 `~/.claude/tasks/<sessionId>/` は共有物なので消してはいけない。
fn tasks_are_shared(app: &App, index: usize) -> bool {
    let target = &app.rows[index].session_id;
    app.rows
        .iter()
        .enumerate()
        .any(|(i, r)| i != index && &r.session_id == target)
}

fn handle_delete(app: &mut App, paths: &Paths, store: &Store, index: usize) {
    let remove_tasks = !tasks_are_shared(app, index);
    let row = &app.rows[index];
    match actions::delete_session(paths, row, remove_tasks) {
        Ok(outcome) => {
            let _ = store.forget_path(&outcome.jsonl.to_string_lossy());
            let tasks = if outcome.tasks_dir.is_some() {
                " (タスクも削除)"
            } else if !remove_tasks {
                " (同一IDの別ファイルが残るためタスクは保持)"
            } else {
                ""
            };
            let msg = format!("削除した: {}{tasks}", outcome.jsonl.display());
            app.remove_row(index);
            app.set_status(msg);
        }
        Err(e) => app.set_status(format!("削除に失敗: {e}")),
    }
}

fn handle_archive(app: &mut App, paths: &Paths, store: &Store, index: usize) {
    let row = &app.rows[index];
    match actions::archive_session(paths, row) {
        Ok(outcome) => {
            let _ = store.forget_path(&outcome.jsonl.to_string_lossy());
            let dest = outcome
                .moved_to
                .as_ref()
                .map(|p| p.display().to_string())
                .unwrap_or_default();
            app.remove_row(index);
            app.set_status(format!("アーカイブした: {dest}"));
        }
        Err(e) => app.set_status(format!("アーカイブに失敗: {e}")),
    }
}

fn handle_grep(
    terminal: &mut DefaultTerminal,
    app: &mut App,
    table_state: &mut TableState,
    needle: String,
) -> Result<()> {
    app.set_status(format!("「{needle}」を全文検索中..."));
    terminal.draw(|f| ui::draw(f, app, table_state))?;

    let hits = grep::search(&app.rows, &needle);
    app.set_content_hits(needle, hits);
    Ok(())
}

fn handle_recap(terminal: &mut DefaultTerminal, app: &mut App, index: usize) -> Result<()> {
    // claude CLI の出力をそのまま見せるため、いったん TUI を畳む
    ratatui::restore();
    let row = &app.rows[index];
    println!("要約中: {} {}", row.short_id(), row.title);

    match actions::run_recap(row) {
        Ok(text) => println!("\n{text}\n"),
        Err(e) => println!("\n要約に失敗: {e:#}\n"),
    }
    println!("Enter で一覧に戻る");
    let mut line = String::new();
    let _ = std::io::stdin().read_line(&mut line);

    *terminal = ratatui::init();
    terminal.clear()?;
    app.set_status("要約を表示した");
    Ok(())
}

fn handle_add_tag(app: &mut App, store: &Store, index: usize, tag: String) {
    let session_id = app.rows[index].session_id.clone();
    match store.add_tag(&session_id, &tag) {
        Ok(()) => {
            let mut tags = app.rows[index].tags.clone();
            let tag = tag.trim().to_string();
            if !tags.contains(&tag) {
                tags.push(tag.clone());
                tags.sort();
            }
            app.set_tags(index, tags);
            app.set_status(format!("タグを付けた: {tag}"));
        }
        Err(e) => app.set_status(format!("タグ付けに失敗: {e}")),
    }
}

/// サブエージェント記録の表示/非表示を切り替えて読み直す (テスト可能な本体)。
///
/// 除外設定を変えると走査対象そのものが変わるため jsonl を読み直す必要がある。
/// ファイル内容が変わっていなければキャッシュが効くので体感は速い。
fn reload_with_subagents(
    app: &mut App,
    paths: &Paths,
    store: &mut Store,
    options: &mut ScanOptions,
) -> Result<()> {
    let next = !options.include_subagents;
    let next_options = ScanOptions {
        include_subagents: next,
    };
    match loader::load_with(paths, store, next_options, None) {
        Ok(loaded) => {
            *options = next_options;
            app.include_subagents = next;
            let total = loaded.stats.total;
            app.replace_rows(loaded.rows);
            let verb = if next { "表示" } else { "非表示" };
            app.set_status(format!("サブエージェント記録を{verb}にした ({total} 件)"));
        }
        Err(e) => app.set_status(format!("切り替えに失敗: {e}")),
    }
    Ok(())
}

/// イベントループから呼ぶ薄いラッパー。切替中であることを一旦描画してから読み直す。
fn handle_toggle_subagents(
    terminal: &mut DefaultTerminal,
    app: &mut App,
    table_state: &mut TableState,
    paths: &Paths,
    store: &mut Store,
    options: &mut ScanOptions,
) -> Result<()> {
    let verb = if options.include_subagents { "非表示" } else { "表示" };
    app.set_status(format!("サブエージェント記録を{verb}に切り替え中..."));
    terminal.draw(|f| ui::draw(f, app, table_state))?;
    reload_with_subagents(app, paths, store, options)
}

fn handle_clear_tags(app: &mut App, store: &Store, index: usize) {
    let session_id = app.rows[index].session_id.clone();
    match store.clear_tags(&session_id) {
        Ok(()) => {
            app.set_tags(index, Vec::new());
            app.set_status("タグを全解除した");
        }
        Err(e) => app.set_status(format!("タグ解除に失敗: {e}")),
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
    use std::fs;
    use std::path::Path;

    /// jsonl を実際に置いた上で 1 行分の App を組む。
    fn app_with_file(paths: &Paths, session_id: &str, body: &str) -> App {
        app_with_files(paths, &[("-proj", session_id, body)])
    }

    /// 複数の jsonl を置いて App を組む (project_dir, sessionId, 中身)。
    fn app_with_files(paths: &Paths, files: &[(&str, &str, &str)]) -> App {
        let scanned: Vec<ScannedSession> = files
            .iter()
            .enumerate()
            .map(|(i, (proj_dir, session_id, body))| {
                let proj = paths.projects().join(proj_dir);
                fs::create_dir_all(&proj).unwrap();
                let path = proj.join(format!("{session_id}.jsonl"));
                fs::write(&path, body).unwrap();

                let created = Some(
                    std::time::SystemTime::UNIX_EPOCH
                        + std::time::Duration::from_secs(100 - i as u64),
                );
                ScannedSession {
                    target: ScanTarget {
                        path,
                        kind: crate::scan::EntryKind::Session,
                        project_dir: (*proj_dir).into(),
                        file_stem: (*session_id).into(),
                        size: body.len() as u64,
                        mtime_ns: 0,
                        created,
                        modified: None,
                    },
                    session_id: (*session_id).into(),
                    cwd: Some("/tmp".into()),
                    title: "タイトル".into(),
                    title_kind: TitleKind::Ai,
                    first_prompt: None,
                    line_count: 1,
                    created,
                }
            })
            .collect();

        let tasks: HashMap<String, TaskSummary> = HashMap::new();
        let running: HashMap<String, RunningSession> = HashMap::new();
        let wl: HashMap<String, WorklogSummary> = HashMap::new();
        let tags: HashMap<String, Vec<String>> = HashMap::new();
        App::new(rows::build(scanned, &tasks, &running, &wl, &tags))
    }

    fn setup(root: &Path) -> (Paths, Store) {
        let paths = Paths::new(root.join("claude"), root.join("data"));
        let store = Store::open(&paths.store_db()).unwrap();
        (paths, store)
    }

    #[test]
    fn 削除の後始末で行とキャッシュが消える() {
        let tmp = tempfile::tempdir().unwrap();
        let (paths, mut store) = setup(tmp.path());
        let mut app = app_with_file(&paths, "sess-a", "{}\n");
        let path = app.rows[0].path.clone();

        store
            .save_cache(&[(
                path.to_string_lossy().to_string(),
                crate::store::CachedSession {
                    mtime_ns: 0,
                    size: 3,
                    session_id: Some("sess-a".into()),
                    cwd: None,
                    title: "タイトル".into(),
                    title_kind: TitleKind::Ai,
                    first_prompt: None,
                    line_count: 1,
                    jsonl_timestamp_ms: None,
                },
            )])
            .unwrap();

        handle_delete(&mut app, &paths, &store, 0);

        assert!(!path.exists());
        assert_eq!(app.counts(), (0, 0));
        assert!(store.load_cache().unwrap().is_empty());
        assert!(app.status.as_deref().unwrap().contains("削除した"));
    }

    #[test]
    fn 同一IDが複数ファイルにあればタスクを残す() {
        // 実データに同一 sessionId が複数プロジェクトディレクトリへ散らばる例がある。
        // 片方を消しただけで共有のタスクを巻き添えにしない。
        let tmp = tempfile::tempdir().unwrap();
        let (paths, store) = setup(tmp.path());
        let mut app = app_with_files(
            &paths,
            &[("-proj-a", "same-id", "{}\n"), ("-proj-b", "same-id", "{}\n")],
        );

        let task_dir = paths.tasks().join("same-id");
        fs::create_dir_all(&task_dir).unwrap();
        fs::write(task_dir.join("1.json"), "{}").unwrap();

        assert!(tasks_are_shared(&app, 0));
        handle_delete(&mut app, &paths, &store, 0);
        assert!(task_dir.exists(), "共有タスクが消えてしまった");
        assert_eq!(app.counts(), (1, 1));
        assert!(app.status.as_deref().unwrap().contains("タスクは保持"));

        // 最後の 1 本を消すときはタスクも片付ける
        assert!(!tasks_are_shared(&app, 0));
        handle_delete(&mut app, &paths, &store, 0);
        assert!(!task_dir.exists());
        assert_eq!(app.counts(), (0, 0));
    }

    #[test]
    fn 削除に失敗しても一覧は残る() {
        let tmp = tempfile::tempdir().unwrap();
        let (paths, store) = setup(tmp.path());
        let mut app = app_with_file(&paths, "sess-a", "{}\n");
        fs::remove_file(&app.rows[0].path).unwrap(); // 先に消しておく

        handle_delete(&mut app, &paths, &store, 0);
        assert_eq!(app.counts(), (1, 1));
        assert!(app.status.as_deref().unwrap().contains("削除に失敗"));
    }

    #[test]
    fn アーカイブの後始末() {
        let tmp = tempfile::tempdir().unwrap();
        let (paths, store) = setup(tmp.path());
        let mut app = app_with_file(&paths, "sess-a", "中身\n");
        let path = app.rows[0].path.clone();

        handle_archive(&mut app, &paths, &store, 0);

        assert!(!path.exists());
        assert!(paths.projects_archive().join("-proj").join("sess-a.jsonl").exists());
        assert_eq!(app.counts(), (0, 0));
        assert!(app.status.as_deref().unwrap().contains("アーカイブした"));
    }

    #[test]
    fn タグ付けがストアと行の両方に反映される() {
        let tmp = tempfile::tempdir().unwrap();
        let (paths, store) = setup(tmp.path());
        let mut app = app_with_file(&paths, "sess-a", "{}\n");

        handle_add_tag(&mut app, &store, 0, "重要".into());
        assert_eq!(app.rows[0].tags, vec!["重要"]);
        assert!(store.has_tag("sess-a", "重要").unwrap());

        handle_add_tag(&mut app, &store, 0, "WAF".into());
        assert_eq!(app.rows[0].tags, vec!["WAF", "重要"]);

        // 同じタグを二重に付けても増えない
        handle_add_tag(&mut app, &store, 0, "WAF".into());
        assert_eq!(app.rows[0].tags.len(), 2);

        handle_clear_tags(&mut app, &store, 0);
        assert!(app.rows[0].tags.is_empty());
        assert!(!store.has_tag("sess-a", "WAF").unwrap());
    }

    /// 本体セッション 1 本とサブエージェント記録 1 本を実ファイルとして置く。
    fn write_session_with_subagent(paths: &Paths) {
        let proj = paths.projects().join("-proj");
        fs::create_dir_all(&proj).unwrap();
        fs::write(
            proj.join("sess-main.jsonl"),
            concat!(
                r#"{"type":"user","sessionId":"sess-main","cwd":"/tmp","message":{"role":"user","content":"本体の質問"}}"#,
                "\n",
                r#"{"type":"custom-title","customTitle":"本体セッション","sessionId":"sess-main"}"#,
                "\n",
            ),
        )
        .unwrap();

        let sub_dir = proj.join("sess-main").join("subagents");
        fs::create_dir_all(&sub_dir).unwrap();
        fs::write(
            sub_dir.join("agent-abc.jsonl"),
            "{\"type\":\"user\",\"isSidechain\":true,\"sessionId\":\"agent-abc\",\"cwd\":\"/tmp\"}\n",
        )
        .unwrap();
    }

    #[test]
    fn サブエージェント表示を切り替えると再走査して件数が変わる() {
        let tmp = tempfile::tempdir().unwrap();
        let (paths, mut store) = setup(tmp.path());
        write_session_with_subagent(&paths);

        let loaded = loader::load_with(&paths, &mut store, ScanOptions::default(), None).unwrap();
        assert_eq!(loaded.rows.len(), 1, "既定ではサブエージェント記録を含まない");

        let mut app = App::new(loaded.rows);
        app.include_subagents = false;
        let mut options = ScanOptions::default();

        reload_with_subagents(&mut app, &paths, &mut store, &mut options).unwrap();

        assert!(options.include_subagents);
        assert!(app.include_subagents);
        assert_eq!(app.counts(), (2, 2), "サブエージェント記録も表示されるはず");
        assert!(app.status.as_deref().unwrap().contains("表示にした"), "{:?}", app.status);

        // もう一度切り替えると元に戻る
        reload_with_subagents(&mut app, &paths, &mut store, &mut options).unwrap();
        assert!(!options.include_subagents);
        assert!(!app.include_subagents);
        assert_eq!(app.counts(), (1, 1));
        assert!(app.status.as_deref().unwrap().contains("非表示にした"), "{:?}", app.status);
    }

    #[test]
    fn サブエージェント切替でも選択中セッションのカーソルが保たれる() {
        let tmp = tempfile::tempdir().unwrap();
        let (paths, mut store) = setup(tmp.path());
        write_session_with_subagent(&paths);

        let loaded = loader::load_with(&paths, &mut store, ScanOptions::default(), None).unwrap();
        let mut app = App::new(loaded.rows);
        let selected_id = app.selected().unwrap().session_id.clone();
        let mut options = ScanOptions::default();

        reload_with_subagents(&mut app, &paths, &mut store, &mut options).unwrap();

        assert_eq!(app.selected().unwrap().session_id, selected_id);
    }

    #[test]
    fn サブエージェントが1件も無くても切替は落ちない() {
        let tmp = tempfile::tempdir().unwrap();
        let (paths, mut store) = setup(tmp.path());
        let mut app = app_with_file(&paths, "sess-a", "{}\n");
        let mut options = ScanOptions::default();

        reload_with_subagents(&mut app, &paths, &mut store, &mut options).unwrap();
        assert!(options.include_subagents);
        assert_eq!(app.counts().1, 1);
    }

    #[test]
    fn 空一覧でもサブエージェント切替は落ちない() {
        let tmp = tempfile::tempdir().unwrap();
        let (paths, mut store) = setup(tmp.path());
        let mut app = App::new(Vec::new());
        let mut options = ScanOptions::default();

        reload_with_subagents(&mut app, &paths, &mut store, &mut options).unwrap();
        assert_eq!(app.counts(), (0, 0));
        assert!(options.include_subagents);
    }
}
