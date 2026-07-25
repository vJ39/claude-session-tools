//! `sync-to-s3.sh` の移植。
//!
//! cron 実行前提。バケットへ疎通できない (ネットワーク断など) 場合は
//! 何もせず exit 0 で終わる。疎通できたときだけ `~/.claude/projects/` を同期する。
//!
//! 同期は aws CLI の `aws s3 sync --size-only --exclude "*.lock"` 相当。
//! ローカルとリモートのサイズだけを比べ、差分のみアップロードする (削除はしない)。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result};
use aws_sdk_s3::Client;
use aws_sdk_s3::config::timeout::TimeoutConfig;
use aws_sdk_s3::primitives::ByteStream;
use walkdir::WalkDir;

/// 既定の AWS プロファイル (sync-to-s3.sh と同じ)。
pub const DEFAULT_PROFILE: &str = "test";
/// 既定のバケット。
pub const DEFAULT_BUCKET: &str = "yotsuya-test";
/// 既定のキープレフィックス。
pub const DEFAULT_PREFIX: &str = "claude-sessions/projects/";
/// 既定の除外パターン。
pub const DEFAULT_EXCLUDE: &str = "*.lock";
/// 疎通確認のタイムアウト。
pub const PROBE_TIMEOUT: Duration = Duration::from_secs(5);
/// 同時アップロード数。
const UPLOAD_CONCURRENCY: usize = 8;

/// sync-s3 の設定。
#[derive(Debug, Clone)]
pub struct SyncConfig {
    pub profile: String,
    pub bucket: String,
    pub prefix: String,
    pub source: PathBuf,
    pub excludes: Vec<String>,
    pub dry_run: bool,
    pub verbose: bool,
}

/// 同期対象のローカルファイル。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalFile {
    /// source からの相対パス (`/` 区切り)
    pub rel: String,
    pub abs: PathBuf,
    pub size: u64,
}

/// アップロード 1 件。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UploadItem {
    pub key: String,
    pub abs: PathBuf,
    pub size: u64,
    /// リモートに無い (true) / サイズ違い (false)
    pub is_new: bool,
}

/// 同期結果。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SyncOutcome {
    /// 疎通確認に失敗してスキップした
    pub skipped: bool,
    pub local_files: usize,
    pub remote_objects: usize,
    pub uploaded: usize,
    pub bytes: u64,
    pub errors: usize,
}

/// aws CLI の `--exclude` 相当のワイルドカード照合。
///
/// `*` は `/` を含む任意文字列、`?` は任意 1 文字にマッチする
/// (aws CLI のフィルタと同じ扱い)。
pub fn matches_glob(pattern: &str, text: &str) -> bool {
    let p: Vec<char> = pattern.chars().collect();
    let t: Vec<char> = text.chars().collect();
    glob_inner(&p, &t)
}

fn glob_inner(p: &[char], t: &[char]) -> bool {
    // 動的計画法で '*' のバックトラックを避ける
    let (n, m) = (p.len(), t.len());
    let mut dp = vec![false; m + 1];
    dp[0] = true;
    for &pc in p.iter().take(n) {
        if pc == '*' {
            // 直前までの結果を左から累積 OR
            for j in 1..=m {
                dp[j] = dp[j] || dp[j - 1];
            }
        } else {
            // 後ろから更新 (dp[j-1] は前段の値を使う)
            for j in (1..=m).rev() {
                dp[j] = dp[j - 1] && (pc == '?' || pc == t[j - 1]);
            }
            dp[0] = false;
        }
    }
    dp[m]
}

/// 同期元ディレクトリを走査し、除外にかからないファイルを集める。
///
/// `aws s3 sync` はシンボリックリンクを既定でたどる (`--no-follow-symlinks` で無効化)。
/// 挙動を合わせるためこちらもたどる。実データの projects 配下には
/// サブエージェント記録を指すシンボリックリンクが実在する。
/// リンクのループは walkdir がエラーとして返すので、その場は読み飛ばす。
pub fn collect_local_files(root: &Path, excludes: &[String]) -> Result<Vec<LocalFile>> {
    let mut out = Vec::new();
    if !root.is_dir() {
        return Ok(out);
    }
    for entry in WalkDir::new(root).follow_links(true).into_iter().filter_map(Result::ok) {
        if !entry.file_type().is_file() {
            continue;
        }
        let abs = entry.path().to_path_buf();
        let rel = match abs.strip_prefix(root).ok().and_then(|p| p.to_str()) {
            Some(r) => r.replace(std::path::MAIN_SEPARATOR, "/"),
            None => continue,
        };
        if excludes.iter().any(|pat| matches_glob(pat, &rel)) {
            continue;
        }
        let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
        out.push(LocalFile { rel, abs, size });
    }
    out.sort_by(|a, b| a.rel.cmp(&b.rel));
    Ok(out)
}

/// `--size-only` 相当の差分計算。
/// リモートに無いか、サイズが違うものだけアップロード対象にする。
pub fn plan_uploads(
    local: &[LocalFile],
    remote: &HashMap<String, u64>,
    prefix: &str,
) -> Vec<UploadItem> {
    let mut out = Vec::new();
    for f in local {
        let key = format!("{prefix}{}", f.rel);
        match remote.get(&key) {
            Some(&size) if size == f.size => {}
            Some(_) => out.push(UploadItem {
                key,
                abs: f.abs.clone(),
                size: f.size,
                is_new: false,
            }),
            None => out.push(UploadItem {
                key,
                abs: f.abs.clone(),
                size: f.size,
                is_new: true,
            }),
        }
    }
    out
}

/// HTTPS クライアントを組み立てる。
///
/// SDK 既定の TLS バックエンド (aws-lc) は C ビルドが環境依存で壊れるため、
/// ring ベースの rustls を明示して差し込む。
fn https_client() -> aws_sdk_s3::config::SharedHttpClient {
    use aws_smithy_http_client::tls;
    aws_smithy_http_client::Builder::new()
        .tls_provider(tls::Provider::Rustls(
            tls::rustls_provider::CryptoMode::Ring,
        ))
        .build_https()
}

/// バケットへ疎通できるか確かめる (head-bucket・5 秒タイムアウト)。
async fn probe(config: &SyncConfig) -> Result<Client> {
    let base = aws_config::defaults(aws_config::BehaviorVersion::latest())
        .http_client(https_client())
        .profile_name(&config.profile)
        .load()
        .await;

    let probe_conf = aws_sdk_s3::config::Builder::from(&base)
        .timeout_config(
            TimeoutConfig::builder()
                .operation_timeout(PROBE_TIMEOUT)
                .build(),
        )
        .build();
    let probe_client = Client::from_conf(probe_conf);

    probe_client
        .head_bucket()
        .bucket(&config.bucket)
        .send()
        .await
        .context("head-bucket に失敗")?;

    Ok(Client::new(&base))
}

/// プレフィックス配下のオブジェクトを列挙する (key -> size)。
async fn list_remote(client: &Client, bucket: &str, prefix: &str) -> Result<HashMap<String, u64>> {
    let mut out = HashMap::new();
    let mut stream = client
        .list_objects_v2()
        .bucket(bucket)
        .prefix(prefix)
        .into_paginator()
        .send();

    while let Some(page) = stream.next().await {
        let page = page.context("list-objects-v2 に失敗")?;
        for obj in page.contents() {
            if let Some(key) = obj.key() {
                out.insert(key.to_string(), obj.size().unwrap_or(0).max(0) as u64);
            }
        }
    }
    Ok(out)
}

/// sync-s3 本体。
///
/// 疎通できなければ `skipped: true` を返す (呼び出し側は exit 0 にする)。
pub async fn run(config: &SyncConfig) -> Result<SyncOutcome> {
    let client = match probe(config).await {
        Ok(c) => c,
        Err(_) => {
            // ネットワーク断・認証切れ等。cron で騒がないよう静かに終わる
            return Ok(SyncOutcome {
                skipped: true,
                ..Default::default()
            });
        }
    };

    let local = collect_local_files(&config.source, &config.excludes)?;
    let remote = list_remote(&client, &config.bucket, &config.prefix).await?;
    let uploads = plan_uploads(&local, &remote, &config.prefix);

    let mut outcome = SyncOutcome {
        skipped: false,
        local_files: local.len(),
        remote_objects: remote.len(),
        ..Default::default()
    };

    if config.dry_run {
        if config.verbose {
            for u in &uploads {
                println!(
                    "upload: {} -> s3://{}/{} ({})",
                    u.abs.display(),
                    config.bucket,
                    u.key,
                    if u.is_new { "new" } else { "size differs" }
                );
            }
        }
        outcome.uploaded = uploads.len();
        outcome.bytes = uploads.iter().map(|u| u.size).sum();
        return Ok(outcome);
    }

    let semaphore = std::sync::Arc::new(tokio::sync::Semaphore::new(UPLOAD_CONCURRENCY));
    let mut set = tokio::task::JoinSet::new();

    for item in uploads {
        let permit = semaphore.clone().acquire_owned().await?;
        let client = client.clone();
        let bucket = config.bucket.clone();
        let verbose = config.verbose;
        set.spawn(async move {
            let _permit = permit;
            let result = upload_one(&client, &bucket, &item).await;
            match result {
                Ok(()) => {
                    if verbose {
                        println!("upload: {} -> s3://{}/{}", item.abs.display(), bucket, item.key);
                    }
                    Ok(item.size)
                }
                Err(e) => {
                    // aws CLI の --only-show-errors と同じくエラーだけ出す
                    eprintln!("upload failed: {} : {e:#}", item.key);
                    Err(())
                }
            }
        });
    }

    while let Some(joined) = set.join_next().await {
        match joined {
            Ok(Ok(size)) => {
                outcome.uploaded += 1;
                outcome.bytes += size;
            }
            Ok(Err(())) => outcome.errors += 1,
            Err(e) => {
                eprintln!("upload task failed: {e}");
                outcome.errors += 1;
            }
        }
    }

    Ok(outcome)
}

async fn upload_one(client: &Client, bucket: &str, item: &UploadItem) -> Result<()> {
    let body = ByteStream::from_path(&item.abs)
        .await
        .with_context(|| format!("読み込めない: {}", item.abs.display()))?;
    client
        .put_object()
        .bucket(bucket)
        .key(&item.key)
        .body(body)
        .send()
        .await
        .with_context(|| format!("put-object に失敗: {}", item.key))?;
    Ok(())
}

#[cfg(test)]
#[allow(non_snake_case)] // テスト名は日本語で書く
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn ワイルドカード照合() {
        assert!(matches_glob("*.lock", "a.lock"));
        assert!(matches_glob("*.lock", "dir/sub/a.lock"));
        assert!(!matches_glob("*.lock", "a.jsonl"));
        assert!(!matches_glob("*.lock", "a.lock.bak"));
        assert!(matches_glob("*", "なんでも"));
        assert!(matches_glob("a?c", "abc"));
        assert!(!matches_glob("a?c", "ac"));
        assert!(matches_glob("", ""));
        assert!(!matches_glob("", "x"));
        assert!(matches_glob("**", "x/y"));
    }

    #[test]
    fn ローカルファイルを集めてlockを除外する() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        fs::create_dir_all(root.join("proj-a")).unwrap();
        fs::write(root.join("proj-a/s1.jsonl"), "abc").unwrap();
        fs::write(root.join("proj-a/s1.lock"), "x").unwrap();
        fs::write(root.join("top.jsonl"), "12345").unwrap();

        let files = collect_local_files(root, &[DEFAULT_EXCLUDE.to_string()]).unwrap();
        let rels: Vec<&str> = files.iter().map(|f| f.rel.as_str()).collect();
        assert_eq!(rels, vec!["proj-a/s1.jsonl", "top.jsonl"]);
        assert_eq!(files[0].size, 3);
        assert_eq!(files[1].size, 5);
    }

    #[test]
    fn シンボリックリンクもたどる() {
        // aws s3 sync の既定挙動に合わせる
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        fs::create_dir_all(root.join("real")).unwrap();
        fs::write(root.join("real/a.jsonl"), "abcde").unwrap();
        fs::create_dir_all(root.join("link")).unwrap();
        std::os::unix::fs::symlink(root.join("real/a.jsonl"), root.join("link/a.jsonl")).unwrap();

        let files = collect_local_files(root, &[DEFAULT_EXCLUDE.to_string()]).unwrap();
        let rels: Vec<&str> = files.iter().map(|f| f.rel.as_str()).collect();
        assert_eq!(rels, vec!["link/a.jsonl", "real/a.jsonl"]);
        assert!(files.iter().all(|f| f.size == 5), "リンク先の実サイズを見ていない");
    }

    #[test]
    fn リンクのループがあっても止まらない() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        fs::create_dir_all(root.join("d")).unwrap();
        fs::write(root.join("d/a.jsonl"), "x").unwrap();
        // 自分の親を指すループを作る
        std::os::unix::fs::symlink(root.join("d"), root.join("d/loop")).unwrap();

        let files = collect_local_files(root, &[]).unwrap();
        assert!(files.iter().any(|f| f.rel == "d/a.jsonl"));
    }

    #[test]
    fn 同期元が無ければ空() {
        let tmp = tempfile::tempdir().unwrap();
        let files = collect_local_files(&tmp.path().join("無い"), &[]).unwrap();
        assert!(files.is_empty());
    }

    fn lf(rel: &str, size: u64) -> LocalFile {
        LocalFile {
            rel: rel.to_string(),
            abs: PathBuf::from(format!("/local/{rel}")),
            size,
        }
    }

    #[test]
    fn サイズ一致はアップロードしない() {
        let local = vec![lf("a.jsonl", 100)];
        let remote = HashMap::from([("p/a.jsonl".to_string(), 100u64)]);
        assert!(plan_uploads(&local, &remote, "p/").is_empty());
    }

    #[test]
    fn サイズ違いと新規はアップロードする() {
        let local = vec![lf("a.jsonl", 100), lf("b.jsonl", 50), lf("c.jsonl", 7)];
        let remote = HashMap::from([
            ("p/a.jsonl".to_string(), 100u64), // 一致 → 対象外
            ("p/b.jsonl".to_string(), 40u64),  // サイズ違い
        ]);
        let plan = plan_uploads(&local, &remote, "p/");
        assert_eq!(plan.len(), 2);
        assert_eq!(plan[0].key, "p/b.jsonl");
        assert!(!plan[0].is_new);
        assert_eq!(plan[1].key, "p/c.jsonl");
        assert!(plan[1].is_new);
    }

    #[test]
    fn プレフィックス付きのキーになる() {
        let local = vec![lf("-Users-work/s.jsonl", 1)];
        let plan = plan_uploads(&local, &HashMap::new(), DEFAULT_PREFIX);
        assert_eq!(plan[0].key, "claude-sessions/projects/-Users-work/s.jsonl");
    }

    #[test]
    fn 空サイズのファイルも新規なら対象() {
        let local = vec![lf("empty.jsonl", 0)];
        let plan = plan_uploads(&local, &HashMap::new(), "p/");
        assert_eq!(plan.len(), 1);
        // リモートにも 0 バイトであればスキップ
        let remote = HashMap::from([("p/empty.jsonl".to_string(), 0u64)]);
        assert!(plan_uploads(&local, &remote, "p/").is_empty());
    }

    #[test]
    fn 既定値はシェル版と同じ() {
        assert_eq!(DEFAULT_PROFILE, "test");
        assert_eq!(DEFAULT_BUCKET, "yotsuya-test");
        assert_eq!(DEFAULT_PREFIX, "claude-sessions/projects/");
        assert_eq!(DEFAULT_EXCLUDE, "*.lock");
        assert_eq!(PROBE_TIMEOUT, Duration::from_secs(5));
    }
}
