//! エントリポイント。サブコマンドの振り分けだけを行う。

use std::process::ExitCode;

use clap::Parser;

use cst::cli::{Cli, Commands, MergeArgs, SyncArgs};
use cst::merge::{MergeOptions, merge};
use cst::paths::Paths;
use cst::s3sync::{self, SyncConfig};
use cst::scan::ScanOptions;
use cst::tui::{self, PostAction};

fn main() -> ExitCode {
    let cli = Cli::parse();

    let scan_options = ScanOptions {
        include_subagents: cli.include_subagents,
    };
    let result = match cli.command {
        None => run_browser(scan_options),
        Some(Commands::Merge(args)) => run_merge(args),
        Some(Commands::SyncS3(args)) => run_sync(args),
    };

    match result {
        Ok(code) => code,
        Err(e) => {
            eprintln!("{e:#}");
            ExitCode::FAILURE
        }
    }
}

/// TUI セッションブラウザ。
fn run_browser(options: ScanOptions) -> anyhow::Result<ExitCode> {
    let paths = Paths::from_env()?;
    match tui::run(&paths, options)? {
        PostAction::None => Ok(ExitCode::SUCCESS),
        PostAction::Resume(spec) => {
            // TUI は畳んだ後。ここで claude をそのまま起動する
            let status = spec.to_command().status()?;
            Ok(match status.code() {
                Some(0) | None => ExitCode::SUCCESS,
                Some(c) => ExitCode::from(c.clamp(0, 255) as u8),
            })
        }
    }
}

/// merge-session.py 相当。
fn run_merge(args: MergeArgs) -> anyhow::Result<ExitCode> {
    // Python 版と同じく、存在しないパスは実行前に弾く
    for p in [&args.session_a, &args.session_b] {
        if !p.exists() {
            eprintln!("エラー: {} が見つからない", p.display());
            return Ok(ExitCode::FAILURE);
        }
    }

    let opts = MergeOptions {
        dry_run: args.dry_run,
        append: args.append,
    };
    let mut stdout = std::io::stdout().lock();
    match merge(&args.session_a, &args.session_b, opts, &mut stdout) {
        Ok(_) => Ok(ExitCode::SUCCESS),
        Err(e) => {
            eprintln!("{e:#}");
            Ok(ExitCode::FAILURE)
        }
    }
}

/// sync-to-s3.sh 相当。cron 前提でネットワーク断時は静かに成功終了する。
fn run_sync(args: SyncArgs) -> anyhow::Result<ExitCode> {
    let paths = Paths::from_env()?;
    let config = SyncConfig {
        profile: args.resolve_profile(),
        bucket: args.bucket.clone(),
        prefix: args.prefix.clone(),
        source: args.source.clone().unwrap_or_else(|| paths.projects()),
        excludes: args.excludes.clone(),
        dry_run: args.dry_run,
        verbose: args.verbose,
    };

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    let outcome = runtime.block_on(s3sync::run(&config))?;

    if outcome.skipped {
        // 疎通できない = ネットワーク断等。cron を騒がせない
        return Ok(ExitCode::SUCCESS);
    }
    if config.verbose {
        println!(
            "ローカル {} / リモート {} / 転送 {} ({} バイト) / エラー {}",
            outcome.local_files,
            outcome.remote_objects,
            outcome.uploaded,
            outcome.bytes,
            outcome.errors
        );
    }
    Ok(if outcome.errors > 0 {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    })
}
