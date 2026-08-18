//! 各データソース (jsonl / tasks / sessions / worklog / タグ) を突き合わせて一覧行を作る。
//!
//! TUI のイベントループから切り離してあるので、実データを置いたディレクトリを
//! 与えるだけでテストできる。

use std::time::SystemTime;

use anyhow::Result;

use crate::fork;
use crate::paths::Paths;
use crate::rows::{self, SessionRow};
use crate::scan::{self, ScanOptions, ScanStats};
use crate::store::Store;
use crate::{registry, tasks, worklog};

/// 読み込み結果。
pub struct Loaded {
    pub rows: Vec<SessionRow>,
    pub stats: ScanStats,
}

/// 一覧に必要なデータを全部読む (既定の設定)。
pub fn load(
    paths: &Paths,
    store: &mut Store,
    progress: Option<&(dyn Fn(usize, usize) + Sync)>,
) -> Result<Loaded> {
    load_with(paths, store, ScanOptions::default(), progress)
}

/// 走査設定を指定して読む。
pub fn load_with(
    paths: &Paths,
    store: &mut Store,
    options: ScanOptions,
    progress: Option<&(dyn Fn(usize, usize) + Sync)>,
) -> Result<Loaded> {
    let (sessions, stats) = scan::scan_all(&paths.projects(), Some(store), options, progress)?;
    let tasks = tasks::load_all(&paths.tasks());
    let running = registry::load(&paths.sessions());
    let worklog = worklog::load(&paths.worklog_db());
    let tags = store.load_tags()?;

    // fork (resume による分岐) の起源判定には物理ファイルの birthtime を使う。
    // build() が sessions の所有権を消費するので、その前に抜き出しておく
    let birthtimes: Vec<(String, Option<SystemTime>)> = sessions
        .iter()
        .map(|s| (s.session_id.clone(), s.target.created))
        .collect();

    let mut rows = rows::build(sessions, &tasks, &running, &worklog, &tags);

    let shared = store.shared_message_uuids()?;
    let fork_groups = fork::detect_fork_groups(&shared, &birthtimes);
    rows::apply_fork_marks(&mut rows, &fork_groups);

    Ok(Loaded { rows, stats })
}

#[cfg(test)]
#[allow(non_snake_case)] // テスト名は日本語で書く
mod tests {
    use super::*;
    use std::fs;

    fn setup(root: &std::path::Path) -> Paths {
        let paths = Paths::new(root.join("claude"), root.join("data"));

        // セッション jsonl 2 本
        let proj = paths.projects().join("-Users-work--ghq-repo");
        fs::create_dir_all(&proj).unwrap();
        fs::write(
            proj.join("sess-aaaa.jsonl"),
            concat!(
                r#"{"type":"user","sessionId":"sess-aaaa","cwd":"/Users/work/.ghq/repo","message":{"role":"user","content":"WAFの調査をしたい"}}"#,
                "\n",
                r#"{"type":"custom-title","customTitle":"[#55710] WAF調査","sessionId":"sess-aaaa"}"#,
                "\n",
            ),
        )
        .unwrap();
        fs::write(
            proj.join("sess-bbbb.jsonl"),
            concat!(
                r#"{"type":"user","sessionId":"sess-bbbb","cwd":"/Users/work/.ghq/repo","message":{"role":"user","content":"コスト削減の相談"}}"#,
                "\n",
            ),
        )
        .unwrap();

        // タスク
        let task_dir = paths.tasks().join("sess-aaaa");
        fs::create_dir_all(&task_dir).unwrap();
        fs::write(
            task_dir.join("1.json"),
            r#"{"id":"1","subject":"[#55711] 追加チケット","status":"pending"}"#,
        )
        .unwrap();
        fs::write(
            task_dir.join("2.json"),
            r#"{"id":"2","subject":"完了済み","status":"completed"}"#,
        )
        .unwrap();

        // 実行中レジストリ
        fs::create_dir_all(paths.sessions()).unwrap();
        fs::write(
            paths.sessions().join("100.json"),
            r#"{"pid":100,"sessionId":"sess-bbbb","status":"busy","kind":"interactive"}"#,
        )
        .unwrap();

        // worklog
        let conn = rusqlite::Connection::open(paths.worklog_db()).unwrap();
        conn.execute_batch(
            "CREATE TABLE work_log (id INTEGER PRIMARY KEY AUTOINCREMENT, session_id TEXT NOT NULL,
             project TEXT, cwd TEXT, start_ts INTEGER NOT NULL, stop_ts INTEGER NOT NULL,
             elapsed_sec INTEGER NOT NULL, prompt TEXT, ticket TEXT, tag TEXT);
             INSERT INTO work_log (session_id,start_ts,stop_ts,elapsed_sec,ticket)
             VALUES ('sess-aaaa',0,600,600,'55712');",
        )
        .unwrap();
        drop(conn);

        paths
    }

    #[test]
    fn 全データソースを突き合わせて一覧を作る() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = setup(tmp.path());
        let mut store = Store::open(&paths.store_db()).unwrap();
        store.add_tag("sess-aaaa", "重要").unwrap();

        let loaded = load(&paths, &mut store, None).unwrap();
        assert_eq!(loaded.rows.len(), 2);
        assert_eq!(loaded.stats.total, 2);
        assert_eq!(loaded.stats.parsed, 2);

        let a = loaded
            .rows
            .iter()
            .find(|r| r.session_id == "sess-aaaa")
            .unwrap();
        assert_eq!(a.title, "[#55710] WAF調査");
        // タイトル + タスク subject + worklog からチケットが集まる
        assert_eq!(a.tickets, vec![55710, 55711, 55712]);
        assert_eq!(a.format_tasks(), "1/0/1");
        assert_eq!(a.tags, vec!["重要"]);
        assert_eq!(a.worklog.elapsed_sec, 600);
        assert!(!a.is_running());

        let b = loaded
            .rows
            .iter()
            .find(|r| r.session_id == "sess-bbbb")
            .unwrap();
        assert!(b.is_running());
        // タイトル行が無いので最初の user メッセージ冒頭にフォールバックする
        assert_eq!(b.title, "コスト削減の相談");
        assert_eq!(b.title_kind, crate::session::TitleKind::FirstPrompt);
        assert!(b.tickets.is_empty());
    }

    #[test]
    fn 二回目の読み込みはキャッシュが効く() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = setup(tmp.path());
        let mut store = Store::open(&paths.store_db()).unwrap();

        load(&paths, &mut store, None).unwrap();
        let second = load(&paths, &mut store, None).unwrap();
        assert_eq!(second.stats.cached, 2);
        assert_eq!(second.stats.parsed, 0);
        assert_eq!(second.rows.len(), 2);
    }

    #[test]
    fn データが何も無くても空で返る() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = Paths::new(tmp.path().join("claude"), tmp.path().join("data"));
        let mut store = Store::open(&paths.store_db()).unwrap();
        let loaded = load(&paths, &mut store, None).unwrap();
        assert!(loaded.rows.is_empty());
        assert_eq!(loaded.stats.total, 0);
    }

    /// resume で分岐した (uuid を共有する) 2 セッションを実ファイルとして置く。
    /// 先に作った方が birthtime が古くなるので起源になる。
    fn setup_fork(root: &std::path::Path) -> Paths {
        let paths = Paths::new(root.join("claude"), root.join("data"));
        let proj = paths.projects().join("-Users-work--ghq-repo");
        fs::create_dir_all(&proj).unwrap();

        fs::write(
            proj.join("sess-origin.jsonl"),
            concat!(
                r#"{"type":"user","sessionId":"sess-origin","cwd":"/Users/work/.ghq/repo","uuid":"shared-1","message":{"role":"user","content":"最初の質問"}}"#,
                "\n",
            ),
        )
        .unwrap();
        // birthtime を区別するため少し間を空けてから 2 本目を作る
        std::thread::sleep(std::time::Duration::from_millis(20));
        fs::write(
            proj.join("sess-child.jsonl"),
            concat!(
                r#"{"type":"user","sessionId":"sess-child","cwd":"/Users/work/.ghq/repo","uuid":"shared-1","message":{"role":"user","content":"最初の質問"}}"#,
                "\n",
                r#"{"type":"assistant","sessionId":"sess-child","uuid":"child-only","message":{"role":"assistant","content":"分岐後の返答"}}"#,
                "\n",
            ),
        )
        .unwrap();

        paths
    }

    #[test]
    fn forkしたセッションに起源マークが付く() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = setup_fork(tmp.path());
        let mut store = Store::open(&paths.store_db()).unwrap();

        let loaded = load(&paths, &mut store, None).unwrap();
        let origin = loaded.rows.iter().find(|r| r.session_id == "sess-origin").unwrap();
        let child = loaded.rows.iter().find(|r| r.session_id == "sess-child").unwrap();

        let origin_mark = origin.fork.as_ref().expect("起源セッションにも fork 情報が付くはず");
        assert!(origin_mark.is_root);
        assert_eq!(origin_mark.group_members, vec!["sess-child".to_string()]);

        let child_mark = child.fork.as_ref().expect("分岐セッションに fork 情報が付くはず");
        assert!(!child_mark.is_root);
        assert_eq!(child_mark.group_members, vec!["sess-origin".to_string()]);
    }

    #[test]
    fn forkしていないセッションはforkがNoneのまま() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = setup(tmp.path());
        let mut store = Store::open(&paths.store_db()).unwrap();

        let loaded = load(&paths, &mut store, None).unwrap();
        assert!(loaded.rows.iter().all(|r| r.fork.is_none()));
    }

    #[test]
    fn 二回目の読み込みでもfork情報がキャッシュ経由で保たれる() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = setup_fork(tmp.path());
        let mut store = Store::open(&paths.store_db()).unwrap();

        load(&paths, &mut store, None).unwrap();
        // 2回目は session_cache がヒットして再走査されないが、message_uuid は
        // 既に DB にあるので fork 判定は変わらず効くはず
        let second = load(&paths, &mut store, None).unwrap();
        assert_eq!(second.stats.parsed, 0, "2回目はキャッシュヒットで再走査されない");

        let origin = second.rows.iter().find(|r| r.session_id == "sess-origin").unwrap();
        assert!(origin.fork.as_ref().unwrap().is_root);
    }
}
