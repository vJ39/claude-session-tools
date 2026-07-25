//! 本ツール専用の SQLite ストア。
//!
//! - `session_cache` … jsonl 走査結果のキャッシュ (mtime + size が一致すれば再走査しない)
//! - `session_tag` … 手動タグ。jsonl 自体は一切編集しない

use std::collections::HashMap;
use std::path::Path;

use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension, params};

use crate::session::{JsonlMeta, TitleKind};

/// キャッシュ 1 件。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CachedSession {
    pub mtime_ns: i64,
    pub size: i64,
    pub session_id: Option<String>,
    pub cwd: Option<String>,
    pub title: String,
    pub title_kind: TitleKind,
    pub first_prompt: Option<String>,
    pub line_count: i64,
}

impl CachedSession {
    /// 走査結果から作る。
    pub fn from_meta(meta: &JsonlMeta, mtime_ns: i64, size: i64) -> Self {
        let (title, kind) = meta.title();
        Self {
            mtime_ns,
            size,
            session_id: meta.session_id.clone(),
            cwd: meta.cwd.clone(),
            title,
            title_kind: kind,
            first_prompt: meta.first_prompt.clone(),
            line_count: meta.line_count as i64,
        }
    }
}

/// ストア。
pub struct Store {
    conn: Connection,
}

impl Store {
    /// ファイルを開く (無ければ作る)。
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)
                .with_context(|| format!("ディレクトリを作成できない: {}", dir.display()))?;
        }
        let conn = Connection::open(path)
            .with_context(|| format!("ストアを開けない: {}", path.display()))?;
        let store = Self { conn };
        store.migrate()?;
        Ok(store)
    }

    /// メモリ上に開く (テスト用)。
    pub fn open_in_memory() -> Result<Self> {
        let store = Self {
            conn: Connection::open_in_memory()?,
        };
        store.migrate()?;
        Ok(store)
    }

    fn migrate(&self) -> Result<()> {
        self.conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             CREATE TABLE IF NOT EXISTS session_cache (
                path         TEXT PRIMARY KEY,
                mtime_ns     INTEGER NOT NULL,
                size         INTEGER NOT NULL,
                session_id   TEXT,
                cwd          TEXT,
                title        TEXT NOT NULL,
                title_kind   TEXT NOT NULL,
                first_prompt TEXT,
                line_count   INTEGER NOT NULL
             );
             CREATE TABLE IF NOT EXISTS session_tag (
                session_id TEXT NOT NULL,
                tag        TEXT NOT NULL,
                PRIMARY KEY (session_id, tag)
             );",
        )?;
        Ok(())
    }

    /// キャッシュを全件読む (path をキーに)。
    pub fn load_cache(&self) -> Result<HashMap<String, CachedSession>> {
        let mut stmt = self.conn.prepare(
            "SELECT path, mtime_ns, size, session_id, cwd, title, title_kind, first_prompt, line_count
             FROM session_cache",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                CachedSession {
                    mtime_ns: r.get(1)?,
                    size: r.get(2)?,
                    session_id: r.get(3)?,
                    cwd: r.get(4)?,
                    title: r.get(5)?,
                    title_kind: TitleKind::from_tag(&r.get::<_, String>(6)?),
                    first_prompt: r.get(7)?,
                    line_count: r.get(8)?,
                },
            ))
        })?;
        let mut out = HashMap::new();
        for row in rows {
            let (path, cached) = row?;
            out.insert(path, cached);
        }
        Ok(out)
    }

    /// キャッシュをまとめて書く。
    pub fn save_cache(&mut self, entries: &[(String, CachedSession)]) -> Result<()> {
        let tx = self.conn.transaction()?;
        {
            let mut stmt = tx.prepare(
                "INSERT INTO session_cache
                   (path, mtime_ns, size, session_id, cwd, title, title_kind, first_prompt, line_count)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)
                 ON CONFLICT(path) DO UPDATE SET
                   mtime_ns=excluded.mtime_ns, size=excluded.size,
                   session_id=excluded.session_id, cwd=excluded.cwd,
                   title=excluded.title, title_kind=excluded.title_kind,
                   first_prompt=excluded.first_prompt, line_count=excluded.line_count",
            )?;
            for (path, c) in entries {
                stmt.execute(params![
                    path,
                    c.mtime_ns,
                    c.size,
                    c.session_id,
                    c.cwd,
                    c.title,
                    c.title_kind.as_str(),
                    c.first_prompt,
                    c.line_count,
                ])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// 消えたファイルのキャッシュを落とす。
    pub fn prune_cache(&mut self, alive_paths: &[String]) -> Result<usize> {
        let alive: std::collections::HashSet<&str> =
            alive_paths.iter().map(String::as_str).collect();
        let existing: Vec<String> = {
            let mut stmt = self.conn.prepare("SELECT path FROM session_cache")?;
            let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
            rows.collect::<Result<Vec<_>, _>>()?
        };
        let dead: Vec<String> = existing
            .into_iter()
            .filter(|p| !alive.contains(p.as_str()))
            .collect();

        let tx = self.conn.transaction()?;
        {
            let mut stmt = tx.prepare("DELETE FROM session_cache WHERE path = ?1")?;
            for p in &dead {
                stmt.execute([p])?;
            }
        }
        tx.commit()?;
        Ok(dead.len())
    }

    /// 1 件分のキャッシュを消す (削除・アーカイブ時)。
    pub fn forget_path(&self, path: &str) -> Result<()> {
        self.conn
            .execute("DELETE FROM session_cache WHERE path = ?1", [path])?;
        Ok(())
    }

    // ---- タグ ----

    /// 全タグを sessionId ごとに返す。
    pub fn load_tags(&self) -> Result<HashMap<String, Vec<String>>> {
        let mut stmt = self
            .conn
            .prepare("SELECT session_id, tag FROM session_tag ORDER BY session_id, tag")?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
        let mut out: HashMap<String, Vec<String>> = HashMap::new();
        for row in rows {
            let (sid, tag) = row?;
            out.entry(sid).or_default().push(tag);
        }
        Ok(out)
    }

    /// タグを付ける (既にあれば何もしない)。
    pub fn add_tag(&self, session_id: &str, tag: &str) -> Result<()> {
        let tag = tag.trim();
        if tag.is_empty() {
            return Ok(());
        }
        self.conn.execute(
            "INSERT OR IGNORE INTO session_tag (session_id, tag) VALUES (?1, ?2)",
            params![session_id, tag],
        )?;
        Ok(())
    }

    /// タグを外す。
    pub fn remove_tag(&self, session_id: &str, tag: &str) -> Result<()> {
        self.conn.execute(
            "DELETE FROM session_tag WHERE session_id = ?1 AND tag = ?2",
            params![session_id, tag],
        )?;
        Ok(())
    }

    /// セッションのタグを全部外す。
    pub fn clear_tags(&self, session_id: &str) -> Result<()> {
        self.conn
            .execute("DELETE FROM session_tag WHERE session_id = ?1", [session_id])?;
        Ok(())
    }

    /// タグが 1 つでもあるか。
    pub fn has_tag(&self, session_id: &str, tag: &str) -> Result<bool> {
        let found: Option<i64> = self
            .conn
            .query_row(
                "SELECT 1 FROM session_tag WHERE session_id = ?1 AND tag = ?2",
                params![session_id, tag],
                |r| r.get(0),
            )
            .optional()?;
        Ok(found.is_some())
    }
}

#[cfg(test)]
#[allow(non_snake_case)] // テスト名は日本語で書く
mod tests {
    use super::*;

    fn sample(title: &str) -> CachedSession {
        CachedSession {
            mtime_ns: 1,
            size: 2,
            session_id: Some("s1".into()),
            cwd: Some("/x".into()),
            title: title.into(),
            title_kind: TitleKind::Custom,
            first_prompt: Some("最初".into()),
            line_count: 3,
        }
    }

    #[test]
    fn キャッシュを保存して読み戻せる() {
        let mut s = Store::open_in_memory().unwrap();
        s.save_cache(&[("/p/a.jsonl".into(), sample("タイトル"))]).unwrap();
        let loaded = s.load_cache().unwrap();
        assert_eq!(loaded["/p/a.jsonl"], sample("タイトル"));
    }

    #[test]
    fn 同じパスは上書きされる() {
        let mut s = Store::open_in_memory().unwrap();
        s.save_cache(&[("/p/a.jsonl".into(), sample("旧"))]).unwrap();
        s.save_cache(&[("/p/a.jsonl".into(), sample("新"))]).unwrap();
        let loaded = s.load_cache().unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded["/p/a.jsonl"].title, "新");
    }

    #[test]
    fn 消えたファイルのキャッシュを落とす() {
        let mut s = Store::open_in_memory().unwrap();
        s.save_cache(&[
            ("/p/a.jsonl".into(), sample("A")),
            ("/p/b.jsonl".into(), sample("B")),
        ])
        .unwrap();
        let pruned = s.prune_cache(&["/p/a.jsonl".to_string()]).unwrap();
        assert_eq!(pruned, 1);
        let loaded = s.load_cache().unwrap();
        assert_eq!(loaded.len(), 1);
        assert!(loaded.contains_key("/p/a.jsonl"));
    }

    #[test]
    fn 個別にキャッシュを忘れられる() {
        let mut s = Store::open_in_memory().unwrap();
        s.save_cache(&[("/p/a.jsonl".into(), sample("A"))]).unwrap();
        s.forget_path("/p/a.jsonl").unwrap();
        assert!(s.load_cache().unwrap().is_empty());
    }

    #[test]
    fn タグを付け外しできる() {
        let s = Store::open_in_memory().unwrap();
        s.add_tag("s1", "コスト削減").unwrap();
        s.add_tag("s1", "WAF").unwrap();
        s.add_tag("s1", "WAF").unwrap(); // 重複は無視
        s.add_tag("s2", "調査").unwrap();

        let tags = s.load_tags().unwrap();
        assert_eq!(tags["s1"], vec!["WAF", "コスト削減"]);
        assert_eq!(tags["s2"], vec!["調査"]);
        assert!(s.has_tag("s1", "WAF").unwrap());

        s.remove_tag("s1", "WAF").unwrap();
        assert!(!s.has_tag("s1", "WAF").unwrap());
        s.clear_tags("s1").unwrap();
        assert!(!s.load_tags().unwrap().contains_key("s1"));
    }

    #[test]
    fn 空タグは登録しない() {
        let s = Store::open_in_memory().unwrap();
        s.add_tag("s1", "   ").unwrap();
        assert!(s.load_tags().unwrap().is_empty());
    }

    #[test]
    fn タグは前後空白を落として保存する() {
        let s = Store::open_in_memory().unwrap();
        s.add_tag("s1", "  重要  ").unwrap();
        assert_eq!(s.load_tags().unwrap()["s1"], vec!["重要"]);
    }

    #[test]
    fn ファイルとして開ける() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("sub").join("cst.db");
        let mut s = Store::open(&path).unwrap();
        s.save_cache(&[("/p/a.jsonl".into(), sample("A"))]).unwrap();
        drop(s);
        let s2 = Store::open(&path).unwrap();
        assert_eq!(s2.load_cache().unwrap().len(), 1);
    }
}
