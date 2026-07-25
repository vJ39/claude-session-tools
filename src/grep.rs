//! jsonl 全文へのキーワード検索 (内容検索してジャンプ)。
//!
//! ヒットしたセッションの集合を返し、一覧側の絞り込みに重ねる。

use std::collections::HashSet;
use std::io::{BufRead, BufReader};
use std::path::Path;

use crate::rows::SessionRow;

/// 1 ファイル内にキーワードが含まれるか (大文字小文字を区別しない)。
///
/// 行単位で読むので巨大ファイルでもメモリに載せない。
pub fn file_contains(path: &Path, needle_lower: &str) -> bool {
    if needle_lower.is_empty() {
        return true;
    }
    let file = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(_) => return false,
    };
    let mut reader = BufReader::with_capacity(256 * 1024, file);
    let mut buf = Vec::new();
    loop {
        buf.clear();
        match reader.read_until(b'\n', &mut buf) {
            Ok(0) => return false,
            Ok(_) => {}
            Err(_) => return false,
        }
        // UTF-8 として読めない行は飛ばす
        if let Ok(s) = std::str::from_utf8(&buf)
            && s.to_lowercase().contains(needle_lower)
        {
            return true;
        }
    }
}

/// 一覧に載っている全セッションを対象にキーワード検索する。
///
/// 戻り値はヒットした sessionId の集合。
pub fn search(rows: &[SessionRow], needle: &str) -> HashSet<String> {
    let needle_lower = needle.trim().to_lowercase();
    if needle_lower.is_empty() {
        return rows.iter().map(|r| r.session_id.clone()).collect();
    }

    let threads = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
        .min(rows.len().max(1));

    let next = std::sync::atomic::AtomicUsize::new(0);
    let total = rows.len();
    let mut hits = HashSet::new();

    std::thread::scope(|scope| {
        let mut handles = Vec::with_capacity(threads);
        for _ in 0..threads {
            let next = &next;
            let needle_lower = needle_lower.as_str();
            handles.push(scope.spawn(move || {
                let mut local = Vec::new();
                loop {
                    let i = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    if i >= total {
                        break;
                    }
                    if file_contains(&rows[i].path, needle_lower) {
                        local.push(rows[i].session_id.clone());
                    }
                }
                local
            }));
        }
        for h in handles {
            if let Ok(part) = h.join() {
                hits.extend(part);
            }
        }
    });

    hits
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
    use std::path::PathBuf;

    fn make_rows(dir: &Path, files: &[(&str, &str)]) -> Vec<SessionRow> {
        let scanned: Vec<ScannedSession> = files
            .iter()
            .enumerate()
            .map(|(i, (id, body))| {
                let path = dir.join(format!("{id}.jsonl"));
                fs::write(&path, body).unwrap();
                let created = Some(
                    std::time::SystemTime::UNIX_EPOCH
                        + std::time::Duration::from_secs(100 - i as u64),
                );
                ScannedSession {
                    target: ScanTarget {
                        path: PathBuf::from(&path),
                        kind: crate::scan::EntryKind::Session,
                        project_dir: "proj".into(),
                        file_stem: (*id).into(),
                        size: body.len() as u64,
                        mtime_ns: 0,
                        created,
                        modified: None,
                    },
                    session_id: (*id).into(),
                    cwd: Some("/tmp".into()),
                    title: (*id).into(),
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
        rows::build(scanned, &tasks, &running, &wl, &tags)
    }

    #[test]
    fn 本文にヒットしたセッションを返す() {
        let tmp = tempfile::tempdir().unwrap();
        let rows = make_rows(
            tmp.path(),
            &[
                ("s1", "{\"content\":\"CrateDB の移行について\"}\n"),
                ("s2", "{\"content\":\"WAF のボット対策\"}\n"),
                ("s3", "{\"content\":\"CrateDB のベンチ\"}\n"),
            ],
        );
        let hits = search(&rows, "CrateDB");
        assert_eq!(hits.len(), 2);
        assert!(hits.contains("s1"));
        assert!(hits.contains("s3"));
        assert!(!hits.contains("s2"));
    }

    #[test]
    fn 大文字小文字を区別しない() {
        let tmp = tempfile::tempdir().unwrap();
        let rows = make_rows(tmp.path(), &[("s1", "{\"x\":\"CrateDB\"}\n")]);
        assert!(search(&rows, "cratedb").contains("s1"));
        assert!(search(&rows, "CRATEDB").contains("s1"));
    }

    #[test]
    fn 日本語も検索できる() {
        let tmp = tempfile::tempdir().unwrap();
        let rows = make_rows(tmp.path(), &[("s1", "{\"x\":\"コスト削減の相談\"}\n")]);
        assert!(search(&rows, "コスト削減").contains("s1"));
        assert!(search(&rows, "存在しない").is_empty());
    }

    #[test]
    fn 空クエリは全件ヒット扱い() {
        let tmp = tempfile::tempdir().unwrap();
        let rows = make_rows(tmp.path(), &[("s1", "a\n"), ("s2", "b\n")]);
        assert_eq!(search(&rows, "   ").len(), 2);
    }

    #[test]
    fn 複数行にまたがっても行内で見つかれば拾う() {
        let tmp = tempfile::tempdir().unwrap();
        let rows = make_rows(tmp.path(), &[("s1", "一行目\n二行目にキーワード\n三行目\n")]);
        assert!(search(&rows, "キーワード").contains("s1"));
    }

    #[test]
    fn 読めないファイルはヒットしない() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(!file_contains(&tmp.path().join("無い.jsonl"), "x"));
    }

    #[test]
    fn 行をまたぐ文字列にはヒットしない() {
        let tmp = tempfile::tempdir().unwrap();
        let rows = make_rows(tmp.path(), &[("s1", "前半\n後半\n")]);
        assert!(search(&rows, "前半後半").is_empty());
    }
}
