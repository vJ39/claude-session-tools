//! `~/.claude/worklog.db` (SQLite) の読み取り。
//!
//! 一覧のチケット番号補完と、セッションごとの作業時間表示に使う。書き込みはしない。

use std::collections::HashMap;
use std::path::Path;

use anyhow::Result;
use rusqlite::{Connection, OpenFlags};

use crate::ticket;

/// セッション単位の作業ログ集計。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WorklogSummary {
    /// 合計作業秒数
    pub elapsed_sec: i64,
    /// 記録ブロック数
    pub blocks: i64,
    /// worklog 側に記録された Redmine チケット番号
    pub tickets: Vec<u64>,
}

impl WorklogSummary {
    /// `1h23m` 形式。0 なら空文字。
    pub fn format_elapsed(&self) -> String {
        format_duration(self.elapsed_sec)
    }
}

/// 秒数を `1h23m` / `12m` / `45s` 形式にする。
pub fn format_duration(sec: i64) -> String {
    if sec <= 0 {
        return String::new();
    }
    let h = sec / 3600;
    let m = (sec % 3600) / 60;
    if h > 0 {
        format!("{h}h{m:02}m")
    } else if m > 0 {
        format!("{m}m")
    } else {
        format!("{sec}s")
    }
}

/// worklog.db を読み取り専用で開き、sessionId ごとに集計する。
///
/// DB が存在しない場合や work_log テーブルが無い場合は空を返す (一覧表示を止めない)。
pub fn load(db_path: &Path) -> HashMap<String, WorklogSummary> {
    try_load(db_path).unwrap_or_default()
}

fn try_load(db_path: &Path) -> Result<HashMap<String, WorklogSummary>> {
    if !db_path.exists() {
        return Ok(HashMap::new());
    }
    let conn = Connection::open_with_flags(db_path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let mut stmt = conn.prepare(
        "SELECT session_id, SUM(elapsed_sec), COUNT(*), GROUP_CONCAT(DISTINCT ticket) \
         FROM work_log GROUP BY session_id",
    )?;
    let rows = stmt.query_map([], |row| {
        let session_id: String = row.get(0)?;
        let elapsed: Option<i64> = row.get(1)?;
        let blocks: i64 = row.get(2)?;
        let tickets: Option<String> = row.get(3)?;
        Ok((session_id, elapsed.unwrap_or(0), blocks, tickets))
    })?;

    let mut out = HashMap::new();
    for row in rows {
        let (session_id, elapsed, blocks, tickets_raw) = row?;
        let mut tickets = Vec::new();
        if let Some(raw) = tickets_raw {
            for part in raw.split(',') {
                if let Some(n) = ticket::parse_worklog_ticket(part)
                    && !tickets.contains(&n)
                {
                    tickets.push(n);
                }
            }
        }
        tickets.sort_unstable();
        out.insert(
            session_id,
            WorklogSummary {
                elapsed_sec: elapsed,
                blocks,
                tickets,
            },
        );
    }
    Ok(out)
}

#[cfg(test)]
#[allow(non_snake_case)] // テスト名は日本語で書く
mod tests {
    use super::*;

    fn make_db(path: &Path) -> Connection {
        let conn = Connection::open(path).unwrap();
        conn.execute_batch(
            "CREATE TABLE work_log (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                session_id TEXT NOT NULL, project TEXT, cwd TEXT,
                start_ts INTEGER NOT NULL, stop_ts INTEGER NOT NULL,
                elapsed_sec INTEGER NOT NULL, prompt TEXT, ticket TEXT, tag TEXT);",
        )
        .unwrap();
        conn
    }

    #[test]
    fn セッションごとに合計する() {
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("worklog.db");
        let conn = make_db(&db);
        conn.execute(
            "INSERT INTO work_log (session_id, start_ts, stop_ts, elapsed_sec, ticket) VALUES ('s1',0,100,100,'55710')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO work_log (session_id, start_ts, stop_ts, elapsed_sec, ticket) VALUES ('s1',0,200,200,'55711')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO work_log (session_id, start_ts, stop_ts, elapsed_sec, ticket) VALUES ('s2',0,50,50,'23')",
            [],
        )
        .unwrap();
        drop(conn);

        let all = load(&db);
        assert_eq!(all["s1"].elapsed_sec, 300);
        assert_eq!(all["s1"].blocks, 2);
        assert_eq!(all["s1"].tickets, vec![55710, 55711]);
        // 連番 ID (10000 未満) は Redmine 番号として採らない
        assert!(all["s2"].tickets.is_empty());
        assert_eq!(all["s2"].elapsed_sec, 50);
    }

    #[test]
    fn dbが無ければ空() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(load(&tmp.path().join("無い.db")).is_empty());
    }

    #[test]
    fn テーブルが無くても落ちない() {
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("empty.db");
        Connection::open(&db).unwrap();
        assert!(load(&db).is_empty());
    }

    #[test]
    fn 経過秒の整形() {
        assert_eq!(format_duration(0), "");
        assert_eq!(format_duration(-5), "");
        assert_eq!(format_duration(45), "45s");
        assert_eq!(format_duration(60), "1m");
        assert_eq!(format_duration(3600), "1h00m");
        assert_eq!(format_duration(5000), "1h23m");
    }
}
