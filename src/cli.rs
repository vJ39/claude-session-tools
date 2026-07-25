//! コマンドライン定義。
//!
//! サブコマンド無しで TUI セッションブラウザ、`merge` / `sync-s3` を別途提供する。

use std::path::PathBuf;

use clap::{Parser, Subcommand};

/// Claude Code のセッションを一覧・検索・操作する CLI。
#[derive(Debug, Parser)]
#[command(
    name = "cst",
    version,
    about = "Claude Code セッション管理ツール",
    long_about = "サブコマンド無しで実行すると TUI セッションブラウザを起動する。"
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Commands>,

    /// サブエージェント記録 (`<sessionId>/subagents/agent-*.jsonl`) も一覧に含める。
    /// 既定では本体のセッションだけを出す
    #[arg(long)]
    pub include_subagents: bool,
}

#[derive(Debug, Subcommand)]
pub enum Commands {
    /// セッションBの会話をセッションAへ取り込む
    Merge(MergeArgs),
    /// ~/.claude/projects/ を S3 へバックアップする
    SyncS3(SyncArgs),
}

/// merge サブコマンドの引数 (merge-session.py と同じ並び)。
#[derive(Debug, clap::Args)]
pub struct MergeArgs {
    /// 取り込み先のセッション jsonl
    pub session_a: PathBuf,
    /// 取り込み元のセッション jsonl
    pub session_b: PathBuf,
    /// Aの末尾に追加する (既定はAの先頭へ挿入)
    #[arg(long)]
    pub append: bool,
    /// 書き込まずに件数とプレビューだけ表示する
    #[arg(long)]
    pub dry_run: bool,
}

/// sync-s3 サブコマンドの引数。既定値は sync-to-s3.sh と同じ。
#[derive(Debug, clap::Args)]
pub struct SyncArgs {
    /// AWS プロファイル (環境変数 AWS_PROFILE があればそちらを優先)
    #[arg(long)]
    pub profile: Option<String>,
    /// 同期先バケット
    #[arg(long, default_value = crate::s3sync::DEFAULT_BUCKET)]
    pub bucket: String,
    /// 同期先プレフィックス
    #[arg(long, default_value = crate::s3sync::DEFAULT_PREFIX)]
    pub prefix: String,
    /// 同期元ディレクトリ (既定は ~/.claude/projects/)
    #[arg(long)]
    pub source: Option<PathBuf>,
    /// 除外パターン (複数指定可)
    #[arg(long = "exclude", default_values_t = [crate::s3sync::DEFAULT_EXCLUDE.to_string()])]
    pub excludes: Vec<String>,
    /// アップロードせず対象件数だけ出す
    #[arg(long)]
    pub dry_run: bool,
    /// 転送内容を表示する (既定はエラーのみ)
    #[arg(long, short)]
    pub verbose: bool,
}

impl SyncArgs {
    /// 実際に使うプロファイルを決める。
    ///
    /// sync-to-s3.sh は `AWS_PROFILE=test` を固定で渡していた。ここでは
    /// 明示指定 > 環境変数 > 既定値 (test) の順で解決し、鍵はコードに持たない。
    pub fn resolve_profile(&self) -> String {
        if let Some(p) = &self.profile {
            return p.clone();
        }
        std::env::var("AWS_PROFILE")
            .ok()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| crate::s3sync::DEFAULT_PROFILE.to_string())
    }
}

#[cfg(test)]
#[allow(non_snake_case)] // テスト名は日本語で書く
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn 定義が壊れていない() {
        Cli::command().debug_assert();
    }

    #[test]
    fn サブコマンド無しはTUI() {
        let cli = Cli::try_parse_from(["cst"]).unwrap();
        assert!(cli.command.is_none());
    }

    #[test]
    fn サブエージェント表示フラグ() {
        let cli = Cli::try_parse_from(["cst"]).unwrap();
        assert!(!cli.include_subagents, "既定では非表示");

        let cli = Cli::try_parse_from(["cst", "--include-subagents"]).unwrap();
        assert!(cli.include_subagents);
        assert!(cli.command.is_none());

        // TUI 専用のフラグなので、他のサブコマンドでは受け付けない
        assert!(Cli::try_parse_from(["cst", "merge", "a", "b", "--include-subagents"]).is_err());
    }

    #[test]
    fn mergeの位置引数() {
        let cli = Cli::try_parse_from(["cst", "merge", "a.jsonl", "b.jsonl"]).unwrap();
        match cli.command.unwrap() {
            Commands::Merge(a) => {
                assert_eq!(a.session_a, PathBuf::from("a.jsonl"));
                assert_eq!(a.session_b, PathBuf::from("b.jsonl"));
                assert!(!a.append);
                assert!(!a.dry_run);
            }
            other => panic!("merge のはず: {other:?}"),
        }
    }

    #[test]
    fn mergeのフラグ() {
        let cli =
            Cli::try_parse_from(["cst", "merge", "a.jsonl", "b.jsonl", "--append", "--dry-run"])
                .unwrap();
        match cli.command.unwrap() {
            Commands::Merge(a) => {
                assert!(a.append);
                assert!(a.dry_run);
            }
            other => panic!("merge のはず: {other:?}"),
        }
    }

    #[test]
    fn mergeのフラグは位置引数の前でも通る() {
        let cli =
            Cli::try_parse_from(["cst", "merge", "--dry-run", "a.jsonl", "b.jsonl"]).unwrap();
        match cli.command.unwrap() {
            Commands::Merge(a) => {
                assert!(a.dry_run);
                assert_eq!(a.session_a, PathBuf::from("a.jsonl"));
            }
            other => panic!("merge のはず: {other:?}"),
        }
    }

    #[test]
    fn merge引数が足りなければエラー() {
        assert!(Cli::try_parse_from(["cst", "merge", "a.jsonl"]).is_err());
        assert!(Cli::try_parse_from(["cst", "merge"]).is_err());
    }

    #[test]
    fn sync_s3の既定値はシェル版と同じ() {
        let cli = Cli::try_parse_from(["cst", "sync-s3"]).unwrap();
        match cli.command.unwrap() {
            Commands::SyncS3(a) => {
                assert_eq!(a.bucket, "yotsuya-test");
                assert_eq!(a.prefix, "claude-sessions/projects/");
                assert_eq!(a.excludes, vec!["*.lock".to_string()]);
                assert!(!a.dry_run);
                assert!(!a.verbose);
            }
            other => panic!("sync-s3 のはず: {other:?}"),
        }
    }

    #[test]
    fn sync_s3の上書き指定() {
        let cli = Cli::try_parse_from([
            "cst", "sync-s3", "--bucket", "other", "--prefix", "p/", "--dry-run", "-v",
        ])
        .unwrap();
        match cli.command.unwrap() {
            Commands::SyncS3(a) => {
                assert_eq!(a.bucket, "other");
                assert_eq!(a.prefix, "p/");
                assert!(a.dry_run);
                assert!(a.verbose);
            }
            other => panic!("sync-s3 のはず: {other:?}"),
        }
    }

    #[test]
    fn プロファイルは明示指定が最優先() {
        let args = SyncArgs {
            profile: Some("explicit".into()),
            bucket: String::new(),
            prefix: String::new(),
            source: None,
            excludes: vec![],
            dry_run: false,
            verbose: false,
        };
        assert_eq!(args.resolve_profile(), "explicit");
    }

    #[test]
    fn プロファイル未指定なら環境変数か既定値() {
        let args = SyncArgs {
            profile: None,
            bucket: String::new(),
            prefix: String::new(),
            source: None,
            excludes: vec![],
            dry_run: false,
            verbose: false,
        };
        // AWS_PROFILE は他テストと競合しうるので、値の有無で分岐して検証する
        match std::env::var("AWS_PROFILE").ok().filter(|s| !s.is_empty()) {
            Some(env) => assert_eq!(args.resolve_profile(), env),
            None => assert_eq!(args.resolve_profile(), "test"),
        }
    }
}
