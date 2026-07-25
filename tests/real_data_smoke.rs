//! 実データ (`~/.claude`) に対する読み取り専用のスモークテスト。
//!
//! 環境依存なので既定では走らせない。手元で確認したいときに明示的に実行する。
//!
//! ```sh
//! cargo test --release --test real_data_smoke -- --ignored --nocapture
//! ```
//!
//! 書き込みは本ツール用のストア (一時ディレクトリ) だけで、
//! `~/.claude` 配下には一切触れない。

use std::time::Instant;

use cst::loader;
use cst::paths::Paths;
use cst::store::Store;

/// 実データを走査して一覧が組み立てられることを確認する。
#[test]
#[ignore = "実データ依存。--ignored で明示実行する"]
fn 実データを走査できる() {
    let env = Paths::from_env().expect("HOME が要る");
    if !env.projects().is_dir() {
        eprintln!("projects が無いのでスキップ: {}", env.projects().display());
        return;
    }

    // ストアだけ一時ディレクトリへ逃がす (実環境のキャッシュを汚さない)
    let tmp = tempfile::tempdir().unwrap();
    let paths = Paths::new(&env.claude_home, tmp.path());
    let mut store = Store::open(&paths.store_db()).unwrap();

    let t0 = Instant::now();
    let first = loader::load(&paths, &mut store, None).unwrap();
    let cold = t0.elapsed();

    let t1 = Instant::now();
    let second = loader::load(&paths, &mut store, None).unwrap();
    let warm = t1.elapsed();

    println!(
        "セッション {} 件 / 初回 {:.2}s (走査 {}) / 2 回目 {:.2}s (キャッシュ {})",
        first.rows.len(),
        cold.as_secs_f64(),
        first.stats.parsed,
        warm.as_secs_f64(),
        second.stats.cached
    );

    assert!(!first.rows.is_empty(), "セッションが 1 件も取れていない");
    assert_eq!(first.rows.len(), second.rows.len());
    // 2 回目は全件キャッシュヒットするはず (走査中に更新された分は除く)
    assert!(
        second.stats.cached >= second.stats.total.saturating_sub(5),
        "キャッシュが効いていない: {:?}",
        second.stats
    );

    // 作成日時降順に並んでいる
    let created: Vec<_> = first.rows.iter().filter_map(|r| r.created).collect();
    assert!(
        created.windows(2).all(|w| w[0] >= w[1]),
        "作成日時降順になっていない"
    );

    // 上位 5 件を目視用に出す
    for row in first.rows.iter().take(5) {
        println!(
            "  {} {} {:>8} {:<12} {}",
            row.short_id(),
            row.format_created(),
            row.format_tasks(),
            row.format_tickets(),
            row.title
        );
    }

    let with_title = first
        .rows
        .iter()
        .filter(|r| r.title != cst::session::UNTITLED)
        .count();
    let with_ticket = first.rows.iter().filter(|r| !r.tickets.is_empty()).count();
    let with_tasks = first.rows.iter().filter(|r| !r.tasks.is_empty()).count();
    let with_cwd = first.rows.iter().filter(|r| r.cwd.is_some()).count();
    println!(
        "タイトルあり {with_title} / チケットあり {with_ticket} / タスクあり {with_tasks} / cwd あり {with_cwd}"
    );
    assert!(with_title > 0, "タイトルが 1 件も取れていない");
    assert!(with_cwd > 0, "cwd が 1 件も取れていない");
}

/// 実データに対する fuzzy 絞り込みが動くことを確認する。
#[test]
#[ignore = "実データ依存。--ignored で明示実行する"]
fn 実データを絞り込める() {
    let env = Paths::from_env().expect("HOME が要る");
    if !env.projects().is_dir() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let paths = Paths::new(&env.claude_home, tmp.path());
    let mut store = Store::open(&paths.store_db()).unwrap();
    let loaded = loader::load(&paths, &mut store, None).unwrap();

    let mut filter = cst::filter::Filter::new();
    let all = filter.apply(&loaded.rows, "", None);
    assert_eq!(all.len(), loaded.rows.len());

    // 適当なクエリで件数が減る (もしくは 0 になる) こと
    let narrowed = filter.apply(&loaded.rows, "claude", None);
    println!("「claude」で {} 件", narrowed.len());
    assert!(narrowed.len() <= all.len());

    // 先頭行の short_id で引けば必ずそのセッションが出る
    if let Some(first) = loaded.rows.first() {
        let hit = filter.apply(&loaded.rows, first.short_id(), None);
        assert!(
            hit.iter().any(|&i| loaded.rows[i].session_id == first.session_id),
            "自分の ID で引けない"
        );
    }
}
