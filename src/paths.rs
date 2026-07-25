//! ツールが参照する各種パスの解決。
//!
//! テストから差し替えられるよう、環境変数からの解決 (`from_env`) と
//! 明示指定 (`new`) を分けている。

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

/// Claude Code のデータ配置と、本ツール自身のデータ配置。
#[derive(Debug, Clone)]
pub struct Paths {
    /// `~/.claude` 相当
    pub claude_home: PathBuf,
    /// 本ツールのストア置き場 (キャッシュ・タグ)
    pub data_dir: PathBuf,
}

impl Paths {
    pub fn new(claude_home: impl Into<PathBuf>, data_dir: impl Into<PathBuf>) -> Self {
        Self {
            claude_home: claude_home.into(),
            data_dir: data_dir.into(),
        }
    }

    /// 環境変数から解決する。
    ///
    /// - `CST_CLAUDE_HOME` … Claude Code のホーム (既定 `$HOME/.claude`)
    /// - `CST_DATA_DIR` … 本ツールのストア (既定 `$HOME/.local/share/cst`)
    pub fn from_env() -> Result<Self> {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .context("環境変数 HOME が設定されていない")?;

        let claude_home = std::env::var_os("CST_CLAUDE_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".claude"));

        let data_dir = std::env::var_os("CST_DATA_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".local").join("share").join("cst"));

        Ok(Self::new(claude_home, data_dir))
    }

    /// セッション jsonl の置き場
    pub fn projects(&self) -> PathBuf {
        self.claude_home.join("projects")
    }

    /// アーカイブ先 (削除より安全な退避先)
    pub fn projects_archive(&self) -> PathBuf {
        self.claude_home.join("projects-archive")
    }

    /// タスク永続化ディレクトリ
    pub fn tasks(&self) -> PathBuf {
        self.claude_home.join("tasks")
    }

    /// 実行中セッションのレジストリ
    pub fn sessions(&self) -> PathBuf {
        self.claude_home.join("sessions")
    }

    /// 作業時間ログ
    pub fn worklog_db(&self) -> PathBuf {
        self.claude_home.join("worklog.db")
    }

    /// 本ツールのストア (セッションキャッシュ + 手動タグ)
    pub fn store_db(&self) -> PathBuf {
        self.data_dir.join("cst.db")
    }

    /// ストア用ディレクトリを作る
    pub fn ensure_data_dir(&self) -> Result<()> {
        std::fs::create_dir_all(&self.data_dir)
            .with_context(|| format!("データディレクトリを作成できない: {}", self.data_dir.display()))
    }
}

/// `claude` 実行ファイル名。`CST_CLAUDE_BIN` で差し替え可能。
pub fn claude_bin() -> String {
    std::env::var("CST_CLAUDE_BIN").unwrap_or_else(|_| "claude".to_string())
}

/// recap で使う軽量モデル。`CST_RECAP_MODEL` で差し替え可能。
pub fn recap_model() -> String {
    std::env::var("CST_RECAP_MODEL").unwrap_or_else(|_| "haiku".to_string())
}

/// パスの最後の要素を文字列で返す (取れなければ空文字)。
pub fn file_name_str(path: &Path) -> &str {
    path.file_name().and_then(|s| s.to_str()).unwrap_or("")
}

#[cfg(test)]
#[allow(non_snake_case)] // テスト名は日本語で書く
mod tests {
    use super::*;

    #[test]
    fn パス組み立てが期待通り() {
        let p = Paths::new("/tmp/ch", "/tmp/data");
        assert_eq!(p.projects(), PathBuf::from("/tmp/ch/projects"));
        assert_eq!(p.projects_archive(), PathBuf::from("/tmp/ch/projects-archive"));
        assert_eq!(p.tasks(), PathBuf::from("/tmp/ch/tasks"));
        assert_eq!(p.sessions(), PathBuf::from("/tmp/ch/sessions"));
        assert_eq!(p.worklog_db(), PathBuf::from("/tmp/ch/worklog.db"));
        assert_eq!(p.store_db(), PathBuf::from("/tmp/data/cst.db"));
    }

    #[test]
    fn ファイル名取得はUTF8以外でも落ちない() {
        assert_eq!(file_name_str(Path::new("/a/b/c.jsonl")), "c.jsonl");
        assert_eq!(file_name_str(Path::new("/")), "");
    }
}
