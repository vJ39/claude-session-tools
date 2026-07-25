//! `~/.claude/projects/` 配下の jsonl 横断走査。
//!
//! 3672 ファイル・3.1GB を毎回全部読むと起動が遅いので、
//! (mtime, size) が変わっていないファイルはストアのキャッシュを使い回す。

use std::fs::Metadata;
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use anyhow::Result;
use walkdir::WalkDir;

use crate::session::{JsonlMeta, TitleKind, scan_jsonl};
use crate::store::{CachedSession, Store};

/// jsonl の種別。
///
/// `projects/` 配下には実際には 3 種類のファイルが混在する。
/// - 直下の `<sessionId>.jsonl` … resume できる本体のセッション
/// - `<sessionId>/subagents/agent-*.jsonl` … サブエージェントの記録
///   (`isSidechain: true`・タイトルもタスクも持たない)
/// - `.trash/` … 削除済み
///
/// 一覧に出すのは本体のセッションだけ。実データでは 3732 件中 3274 件が
/// サブエージェント記録で、全部並べると本体が埋もれる。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    Session,
    Subagent,
    Trash,
}

/// パスから種別を判定する。
pub fn classify(path: &Path, projects_root: &Path) -> EntryKind {
    let rel = path.strip_prefix(projects_root).unwrap_or(path);
    for c in rel.components() {
        match c.as_os_str().to_str() {
            Some("subagents") => return EntryKind::Subagent,
            Some(".trash") => return EntryKind::Trash,
            _ => {}
        }
    }
    EntryKind::Session
}

/// 走査対象のファイル 1 件。
#[derive(Debug, Clone)]
pub struct ScanTarget {
    pub path: PathBuf,
    pub kind: EntryKind,
    /// `projects/<encoded-cwd>/` のディレクトリ名
    pub project_dir: String,
    /// ファイル名 (拡張子を除いたもの) = sessionId
    pub file_stem: String,
    pub size: u64,
    pub mtime_ns: i64,
    /// birthtime。取れない環境では mtime にフォールバックする
    pub created: Option<SystemTime>,
    pub modified: Option<SystemTime>,
}

/// 走査結果 1 件。
#[derive(Debug, Clone)]
pub struct ScannedSession {
    pub target: ScanTarget,
    pub session_id: String,
    pub cwd: Option<String>,
    pub title: String,
    pub title_kind: TitleKind,
    pub first_prompt: Option<String>,
    pub line_count: i64,
}

/// 走査の設定。
#[derive(Debug, Clone, Copy, Default)]
pub struct ScanOptions {
    /// サブエージェント記録も一覧に含める
    pub include_subagents: bool,
}

/// 走査全体のサマリー。
#[derive(Debug, Clone, Default)]
pub struct ScanStats {
    pub total: usize,
    /// キャッシュが使えた件数
    pub cached: usize,
    /// 実際に読み直した件数
    pub parsed: usize,
    /// 一覧から外したサブエージェント記録の件数
    pub skipped_subagents: usize,
}

/// projects 配下の jsonl を列挙する。
///
/// `.bak.<timestamp>` などの拡張子が jsonl でないファイルは対象外。
pub fn list_targets(projects_dir: &Path) -> Vec<ScanTarget> {
    let mut out = Vec::new();
    if !projects_dir.is_dir() {
        return out;
    }

    for entry in WalkDir::new(projects_dir)
        .min_depth(1)
        .follow_links(false)
        .into_iter()
        .filter_map(Result::ok)
    {
        if !entry.file_type().is_file() {
            continue;
        }
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
            continue;
        }
        let file_stem = match path.file_stem().and_then(|s| s.to_str()) {
            Some(s) => s.to_string(),
            None => continue,
        };
        let project_dir = path
            .parent()
            .and_then(|p| p.file_name())
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_string();

        let meta = match entry.metadata() {
            Ok(m) => m,
            Err(_) => continue,
        };
        out.push(ScanTarget {
            path: path.to_path_buf(),
            kind: classify(path, projects_dir),
            project_dir,
            file_stem,
            size: meta.len(),
            mtime_ns: mtime_ns(&meta),
            created: created_time(&meta),
            modified: meta.modified().ok(),
        });
    }

    out
}

fn mtime_ns(meta: &Metadata) -> i64 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok())
        .map(|d| d.as_nanos() as i64)
        .unwrap_or(0)
}

/// birthtime。取れない環境では mtime へフォールバック。
fn created_time(meta: &Metadata) -> Option<SystemTime> {
    meta.created().ok().or_else(|| meta.modified().ok())
}

/// 1 ファイルを読んでメタ情報を取り出す。
pub fn scan_one(path: &Path) -> JsonlMeta {
    match std::fs::File::open(path) {
        Ok(f) => scan_jsonl(BufReader::with_capacity(256 * 1024, f)),
        Err(_) => JsonlMeta::default(),
    }
}

/// 並列に走査する。`store` があればキャッシュを使い、更新分だけ書き戻す。
pub fn scan_all(
    projects_dir: &Path,
    store: Option<&mut Store>,
    options: ScanOptions,
    progress: Option<&(dyn Fn(usize, usize) + Sync)>,
) -> Result<(Vec<ScannedSession>, ScanStats)> {
    let all = list_targets(projects_dir);
    let skipped_subagents = all
        .iter()
        .filter(|t| t.kind == EntryKind::Subagent)
        .count();
    // キャッシュの掃除は「ディスク上に実在するか」で判断する。
    // 除外設定で外しただけのファイルまで捨てると、設定を戻したとき再走査になる
    let on_disk: Vec<String> = all
        .iter()
        .map(|t| t.path.to_string_lossy().to_string())
        .collect();

    // 削除済みは常に除外。サブエージェント記録は既定で除外する
    let targets: Vec<ScanTarget> = all
        .into_iter()
        .filter(|t| match t.kind {
            EntryKind::Session => true,
            EntryKind::Subagent => options.include_subagents,
            EntryKind::Trash => false,
        })
        .collect();

    let mut stats = ScanStats {
        total: targets.len(),
        skipped_subagents: if options.include_subagents {
            0
        } else {
            skipped_subagents
        },
        ..Default::default()
    };

    let cache = match store.as_ref() {
        Some(s) => s.load_cache()?,
        None => Default::default(),
    };

    // キャッシュヒットと再走査対象に振り分ける
    let mut results: Vec<Option<ScannedSession>> = vec![None; targets.len()];
    let mut todo: Vec<usize> = Vec::new();

    for (i, t) in targets.iter().enumerate() {
        let key = t.path.to_string_lossy().to_string();
        match cache.get(&key) {
            Some(c) if c.mtime_ns == t.mtime_ns && c.size == t.size as i64 => {
                stats.cached += 1;
                results[i] = Some(from_cache(t, c));
            }
            _ => todo.push(i),
        }
    }
    stats.parsed = todo.len();

    // 更新分を並列に読む
    let fresh = parse_in_parallel(&targets, &todo, progress);
    let mut to_save: Vec<(String, CachedSession)> = Vec::with_capacity(fresh.len());
    for (idx, meta) in fresh {
        let t = &targets[idx];
        let cached = CachedSession::from_meta(&meta, t.mtime_ns, t.size as i64);
        to_save.push((t.path.to_string_lossy().to_string(), cached.clone()));
        results[idx] = Some(from_cache(t, &cached));
    }

    if let Some(store) = store {
        if !to_save.is_empty() {
            store.save_cache(&to_save)?;
        }
        store.prune_cache(&on_disk)?;
    }

    Ok((results.into_iter().flatten().collect(), stats))
}

fn from_cache(target: &ScanTarget, c: &CachedSession) -> ScannedSession {
    ScannedSession {
        // sessionId が jsonl 側に無い場合はファイル名を採用する
        session_id: c
            .session_id
            .clone()
            .unwrap_or_else(|| target.file_stem.clone()),
        cwd: c.cwd.clone(),
        title: c.title.clone(),
        title_kind: c.title_kind,
        first_prompt: c.first_prompt.clone(),
        line_count: c.line_count,
        target: target.clone(),
    }
}

/// 対象インデックスの集合を複数スレッドで走査する。
fn parse_in_parallel(
    targets: &[ScanTarget],
    todo: &[usize],
    progress: Option<&(dyn Fn(usize, usize) + Sync)>,
) -> Vec<(usize, JsonlMeta)> {
    if todo.is_empty() {
        return Vec::new();
    }

    let threads = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
        .min(todo.len())
        .max(1);

    let next = std::sync::atomic::AtomicUsize::new(0);
    let done = std::sync::atomic::AtomicUsize::new(0);
    let total = todo.len();
    let mut collected: Vec<(usize, JsonlMeta)> = Vec::with_capacity(total);

    std::thread::scope(|scope| {
        let mut handles = Vec::with_capacity(threads);
        for _ in 0..threads {
            let next = &next;
            let done = &done;
            handles.push(scope.spawn(move || {
                let mut local = Vec::new();
                loop {
                    let i = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    if i >= total {
                        break;
                    }
                    let idx = todo[i];
                    let meta = scan_one(&targets[idx].path);
                    local.push((idx, meta));
                    if let Some(cb) = progress {
                        let n = done.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
                        cb(n, total);
                    }
                }
                local
            }));
        }
        for h in handles {
            if let Ok(mut part) = h.join() {
                collected.append(&mut part);
            }
        }
    });

    collected
}

#[cfg(test)]
#[allow(non_snake_case)] // テスト名は日本語で書く
mod tests {
    use super::*;
    use std::fs;

    fn make_projects(root: &Path) {
        let p1 = root.join("-Users-work--ghq-repo-a");
        let p2 = root.join("-Users-work--ghq-repo-b");
        fs::create_dir_all(&p1).unwrap();
        fs::create_dir_all(&p2).unwrap();
        fs::write(
            p1.join("aaaa1111-0000-0000-0000-000000000001.jsonl"),
            concat!(
                r#"{"type":"user","sessionId":"aaaa1111-0000-0000-0000-000000000001","cwd":"/Users/work/.ghq/repo-a","message":{"role":"user","content":"最初の質問"}}"#,
                "\n",
                r#"{"type":"custom-title","customTitle":"[#55710] WAF調査","sessionId":"aaaa1111-0000-0000-0000-000000000001"}"#,
                "\n",
            ),
        )
        .unwrap();
        fs::write(
            p2.join("bbbb2222-0000-0000-0000-000000000002.jsonl"),
            concat!(
                r#"{"type":"user","sessionId":"bbbb2222-0000-0000-0000-000000000002","cwd":"/Users/work/.ghq/repo-b","message":{"role":"user","content":"別件"}}"#,
                "\n",
            ),
        )
        .unwrap();
        // jsonl 以外・バックアップは対象外
        fs::write(p1.join("メモ.txt"), "無関係").unwrap();
        fs::write(p1.join("aaaa1111.jsonl.bak.20260725000000"), "{}").unwrap();
    }

    #[test]
    fn jsonlだけ列挙する() {
        let tmp = tempfile::tempdir().unwrap();
        make_projects(tmp.path());
        let mut targets = list_targets(tmp.path());
        targets.sort_by(|a, b| a.file_stem.cmp(&b.file_stem));
        assert_eq!(targets.len(), 2);
        assert!(targets[0].file_stem.starts_with("aaaa1111"));
        assert_eq!(targets[0].project_dir, "-Users-work--ghq-repo-a");
        assert!(targets[0].size > 0);
    }

    /// サブエージェント記録と .trash を混ぜたツリーを作る。
    fn make_mixed(root: &Path) {
        make_projects(root);
        let sess = root.join("-Users-work--ghq-repo-a").join("aaaa1111-0000-0000-0000-000000000001");
        let sub = sess.join("subagents");
        fs::create_dir_all(&sub).unwrap();
        fs::write(
            sub.join("agent-abc123.jsonl"),
            "{\"type\":\"user\",\"isSidechain\":true,\"agentId\":\"abc123\",\"sessionId\":\"aaaa1111-0000-0000-0000-000000000001\"}\n",
        )
        .unwrap();
        fs::write(
            sub.join("agent-def456.jsonl"),
            "{\"type\":\"user\",\"isSidechain\":true,\"agentId\":\"def456\"}\n",
        )
        .unwrap();
        // ワークフロー配下にネストしたサブエージェント
        let nested = sub.join("workflows").join("wf_1");
        fs::create_dir_all(&nested).unwrap();
        fs::write(nested.join("agent-ghi789.jsonl"), "{\"type\":\"user\"}\n").unwrap();

        let trash = root.join("-Users-work--ghq-repo-a").join(".trash");
        fs::create_dir_all(&trash).unwrap();
        fs::write(trash.join("deleted-session.jsonl"), "{\"type\":\"user\"}\n").unwrap();
    }

    #[test]
    fn パスから種別を判定する() {
        let root = Path::new("/p");
        assert_eq!(classify(Path::new("/p/proj/abc.jsonl"), root), EntryKind::Session);
        // ネストしたプロジェクトディレクトリも本体セッション扱い
        assert_eq!(
            classify(Path::new("/p/proj/nested/abc.jsonl"), root),
            EntryKind::Session
        );
        assert_eq!(
            classify(Path::new("/p/proj/abc/subagents/agent-x.jsonl"), root),
            EntryKind::Subagent
        );
        assert_eq!(
            classify(Path::new("/p/proj/abc/subagents/workflows/wf_1/agent-x.jsonl"), root),
            EntryKind::Subagent
        );
        assert_eq!(
            classify(Path::new("/p/proj/.trash/abc.jsonl"), root),
            EntryKind::Trash
        );
    }

    #[test]
    fn 既定ではサブエージェント記録と削除済みを外す() {
        let tmp = tempfile::tempdir().unwrap();
        make_mixed(tmp.path());

        let (sessions, stats) = scan_all(tmp.path(), None, ScanOptions::default(), None).unwrap();
        assert_eq!(stats.total, 2, "本体セッションだけ残るはず");
        assert_eq!(stats.skipped_subagents, 3);
        assert!(
            sessions.iter().all(|s| !s.target.path.to_string_lossy().contains("subagents")),
            "サブエージェント記録が混ざっている"
        );
        assert!(
            sessions.iter().all(|s| !s.target.path.to_string_lossy().contains(".trash")),
            "削除済みが混ざっている"
        );
    }

    #[test]
    fn オプションでサブエージェント記録も含められる() {
        let tmp = tempfile::tempdir().unwrap();
        make_mixed(tmp.path());

        let opts = ScanOptions { include_subagents: true };
        let (sessions, stats) = scan_all(tmp.path(), None, opts, None).unwrap();
        // 本体 2 + サブエージェント 3 (.trash は常に除外)
        assert_eq!(stats.total, 5);
        assert_eq!(stats.skipped_subagents, 0);
        assert_eq!(
            sessions
                .iter()
                .filter(|s| s.target.kind == EntryKind::Subagent)
                .count(),
            3
        );
    }

    #[test]
    fn 除外してもキャッシュは消えない() {
        // 一度 include して貯めたキャッシュを、除外実行で捨ててしまわないこと
        let tmp = tempfile::tempdir().unwrap();
        make_mixed(tmp.path());
        let mut store = Store::open_in_memory().unwrap();

        let opts = ScanOptions { include_subagents: true };
        scan_all(tmp.path(), Some(&mut store), opts, None).unwrap();
        let cached_all = store.load_cache().unwrap().len();
        assert_eq!(cached_all, 5);

        // 既定 (除外) で実行してもキャッシュは残る
        scan_all(tmp.path(), Some(&mut store), ScanOptions::default(), None).unwrap();
        assert_eq!(store.load_cache().unwrap().len(), 5);

        // include に戻したとき全部キャッシュヒットする
        let (_, stats) = scan_all(tmp.path(), Some(&mut store), opts, None).unwrap();
        assert_eq!(stats.cached, 5);
        assert_eq!(stats.parsed, 0);
    }

    #[test]
    fn projectsが無ければ空() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(list_targets(&tmp.path().join("無い")).is_empty());
    }

    #[test]
    fn 走査して内容を取り出す() {
        let tmp = tempfile::tempdir().unwrap();
        make_projects(tmp.path());
        let (mut sessions, stats) = scan_all(tmp.path(), None, ScanOptions::default(), None).unwrap();
        sessions.sort_by(|a, b| a.session_id.cmp(&b.session_id));

        assert_eq!(stats.total, 2);
        assert_eq!(stats.parsed, 2);
        assert_eq!(stats.cached, 0);
        assert_eq!(sessions[0].title, "[#55710] WAF調査");
        assert_eq!(sessions[0].cwd.as_deref(), Some("/Users/work/.ghq/repo-a"));
        assert_eq!(sessions[0].title_kind, TitleKind::Custom);
        // タイトル行が無いセッションは最初の user メッセージ冒頭にフォールバックする
        assert_eq!(sessions[1].title, "別件");
        assert_eq!(sessions[1].title_kind, TitleKind::FirstPrompt);
    }

    #[test]
    fn 二回目はキャッシュを使う() {
        let tmp = tempfile::tempdir().unwrap();
        make_projects(tmp.path());
        let mut store = Store::open_in_memory().unwrap();

        let (_, first) = scan_all(tmp.path(), Some(&mut store), ScanOptions::default(), None).unwrap();
        assert_eq!(first.parsed, 2);
        assert_eq!(first.cached, 0);

        let (sessions, second) = scan_all(tmp.path(), Some(&mut store), ScanOptions::default(), None).unwrap();
        assert_eq!(second.parsed, 0);
        assert_eq!(second.cached, 2);
        assert_eq!(sessions.len(), 2);
    }

    #[test]
    fn 内容が変わったら読み直す() {
        let tmp = tempfile::tempdir().unwrap();
        make_projects(tmp.path());
        let mut store = Store::open_in_memory().unwrap();
        scan_all(tmp.path(), Some(&mut store), ScanOptions::default(), None).unwrap();

        let target = tmp
            .path()
            .join("-Users-work--ghq-repo-b")
            .join("bbbb2222-0000-0000-0000-000000000002.jsonl");
        let mut body = fs::read_to_string(&target).unwrap();
        body.push_str(
            "{\"type\":\"custom-title\",\"customTitle\":\"後から付けた名前\",\"sessionId\":\"bbbb2222-0000-0000-0000-000000000002\"}\n",
        );
        fs::write(&target, body).unwrap();

        let (sessions, stats) = scan_all(tmp.path(), Some(&mut store), ScanOptions::default(), None).unwrap();
        assert_eq!(stats.parsed, 1);
        let changed = sessions
            .iter()
            .find(|s| s.session_id.starts_with("bbbb2222"))
            .unwrap();
        assert_eq!(changed.title, "後から付けた名前");
    }

    #[test]
    fn 消えたファイルはキャッシュから落ちる() {
        let tmp = tempfile::tempdir().unwrap();
        make_projects(tmp.path());
        let mut store = Store::open_in_memory().unwrap();
        scan_all(tmp.path(), Some(&mut store), ScanOptions::default(), None).unwrap();
        assert_eq!(store.load_cache().unwrap().len(), 2);

        fs::remove_file(
            tmp.path()
                .join("-Users-work--ghq-repo-b")
                .join("bbbb2222-0000-0000-0000-000000000002.jsonl"),
        )
        .unwrap();

        let (sessions, stats) = scan_all(tmp.path(), Some(&mut store), ScanOptions::default(), None).unwrap();
        assert_eq!(stats.total, 1);
        assert_eq!(sessions.len(), 1);
        assert_eq!(store.load_cache().unwrap().len(), 1);
    }

    #[test]
    fn タイトルフォールバックはキャッシュ経由でも保たれる() {
        let tmp = tempfile::tempdir().unwrap();
        let proj = tmp.path().join("-p");
        fs::create_dir_all(&proj).unwrap();
        fs::write(
            proj.join("no-title.jsonl"),
            "{\"type\":\"user\",\"sessionId\":\"no-title\",\"message\":{\"role\":\"user\",\"content\":\"タイトル無しの質問\"}}\n",
        )
        .unwrap();

        let mut store = Store::open_in_memory().unwrap();
        let (sessions, _) = scan_all(tmp.path(), Some(&mut store), ScanOptions::default(), None).unwrap();
        assert_eq!(sessions[0].title, "タイトル無しの質問");
        assert_eq!(sessions[0].title_kind, TitleKind::FirstPrompt);

        // 2 回目はキャッシュ経由 (from_cache) だが同じフォールバック結果になる
        let (sessions, stats) = scan_all(tmp.path(), Some(&mut store), ScanOptions::default(), None).unwrap();
        assert_eq!(stats.cached, 1);
        assert_eq!(sessions[0].title, "タイトル無しの質問");
        assert_eq!(sessions[0].title_kind, TitleKind::FirstPrompt);
    }

    #[test]
    fn sessionIdが無ければファイル名を使う() {
        let tmp = tempfile::tempdir().unwrap();
        let d = tmp.path().join("-p");
        fs::create_dir_all(&d).unwrap();
        fs::write(d.join("no-session-id.jsonl"), "{\"type\":\"system\"}\n").unwrap();
        let (sessions, _) = scan_all(tmp.path(), None, ScanOptions::default(), None).unwrap();
        assert_eq!(sessions[0].session_id, "no-session-id");
    }

    #[test]
    fn 進捗コールバックが呼ばれる() {
        let tmp = tempfile::tempdir().unwrap();
        make_projects(tmp.path());
        let count = std::sync::atomic::AtomicUsize::new(0);
        let cb = |_done: usize, _total: usize| {
            count.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        };
        scan_all(tmp.path(), None, ScanOptions::default(), Some(&cb)).unwrap();
        assert_eq!(count.load(std::sync::atomic::Ordering::Relaxed), 2);
    }
}
