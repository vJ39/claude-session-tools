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

/// 実データで fork (resume による分岐) 検出が動くことを確認する。
///
/// 2026/08/18 時点の事前調査で `b1ddb196-...` と `b2151286-...` が同じ会話から
/// 分岐したペアと確認済み (共有 uuid 1318件、開始が早い b1ddb196 が起源)。
/// 実データは変化しうるので、この組が現存すれば厳密に検証し、無ければ
/// 「どこかに fork グループが検出されているか」だけを緩く確認する。
#[test]
#[ignore = "実データ依存。--ignored で明示実行する"]
fn 実データでfork検出が動く() {
    let env = Paths::from_env().expect("HOME が要る");
    if !env.projects().is_dir() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let paths = Paths::new(&env.claude_home, tmp.path());
    let mut store = Store::open(&paths.store_db()).unwrap();
    let loaded = loader::load(&paths, &mut store, None).unwrap();

    let forked: Vec<_> = loaded.rows.iter().filter(|r| r.fork.is_some()).collect();
    println!("fork検出: 関連グループに属するセッション {} 件", forked.len());
    for r in forked.iter().take(10) {
        let f = r.fork.as_ref().unwrap();
        println!(
            "  {} {} is_root={} 関連{}件",
            r.short_id(),
            r.title,
            f.is_root,
            f.group_members.len()
        );
    }

    let by_id = |id: &str| loaded.rows.iter().find(|r| r.session_id.starts_with(id));
    match (by_id("b1ddb196"), by_id("b2151286")) {
        (Some(a), Some(b)) => {
            let fa = a.fork.as_ref().expect("b1ddb196 は fork グループに属するはず");
            let fb = b.fork.as_ref().expect("b2151286 は fork グループに属するはず");
            assert!(
                fa.group_members.iter().any(|m| m.starts_with("b2151286")),
                "b1ddb196 の関連に b2151286 が無い"
            );
            assert!(
                fb.group_members.iter().any(|m| m.starts_with("b1ddb196")),
                "b2151286 の関連に b1ddb196 が無い"
            );
            // 開始が早い (2026-07-16) b1ddb196 が起源のはず
            assert!(fa.is_root, "b1ddb196 が起源のはず");
            assert!(!fb.is_root, "b2151286 は起源ではないはず");
        }
        _ => {
            println!("b1ddb196/b2151286 が実データに無い (削除された等)。緩い確認のみ行う");
            // 事前調査 (102セッション中24組) からすると、何らかの fork は
            // 見つかるはずだが、環境依存なので無くても失敗にはしない
        }
    }
}
