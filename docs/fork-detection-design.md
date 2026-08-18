# fork検出・起源マーク・関連セッション表示 設計書

## 背景

`claude --resume <id>` で古いセッションを resume すると、選んだ時点までの会話履歴を丸ごと引き継いだ**新しい sessionId のファイル**が生成される。元のセッションと新しいセッションは見た目上ほぼ同じタイトルになるため、一覧に「同じ会話の分岐」が複数行として並び、どれが元でどれが分岐後かが分からなくなる。

これを解消するため、
1. 同じ会話から分岐した関連セッションを検出してグループ化する
2. グループ内で最初に存在していた「起源セッション」にマークを付ける
3. 選択中セッションが属するグループの関連セッション一覧を表示する

## 実データ調査で分かったこと

`~/.claude/projects/` 配下の全 102 セッション jsonl を対象に、各メッセージの `uuid` フィールドが複数セッションにまたがって出現するかを総当たりで確認した。

- 24 組のセッションペアが uuid を共有していた（= fork 関係にある）
- 3 セッションが互いに uuid を共有する 3 方向 fork も存在する（例: `062cdb26` / `f6da54c8` / `9ef3bb4b`）
- fork した者同士は「会話の最初のメッセージの timestamp」が完全に一致する。これは resume 時に過去履歴をコピーするため当然の結果で、**開始 timestamp だけでは起源を区別できない**
- 分岐は会話の途中（後半）で起きることもある（例: `6edf39b4` は `b1ddb196` の 2026-07-31 時点から分岐しているが、共有 uuid 数はそれ以前の全履歴数より大幅に少ない 255 件だった）。これは `/compact` によるコンテキスト圧縮で古いメッセージの uuid が要約に置き換わり失われるためと推測される
- そのため「共有 uuid 数」や「共有集合が時系列で連続した接頭辞になっているか」から**厳密な親子関係**を機械的に特定するのは、compact や多方向分岐が絡むと精度が出ない

## 採用する設計方針（ユーザー確認済み）

- **起源判定**: グループ内でファイルの物理 birthtime（`ScanTarget.created`、取れない環境は mtime にフォールバック）が最も古いものを起源とする
  - リスク: 旧Mac→新Mac移行時の一括コピーのように、ファイルコピーで birthtime がズレる事故が過去に実際にあった（`store.rs` に記録済み）。移行直後は判定を誤る可能性がある点は許容する
- **グループの粒度**: 厳密な親子ツリーは作らず、**フラットなグループ化**に留める。「このセッション群は互いに関連している」「起源はどれか」だけを扱い、「誰が誰から直接分岐したか」は推定しない

## Fork 検出ロジック

### 1. データ収集

各セッション jsonl の走査時（`session::scan_jsonl`）に、既存の `line_count` / `first_prompt` 抽出と同じループの中で、メッセージ行の `uuid` と `timestamp` も収集する。

```rust
// JsonlMeta に追加
pub message_uuids: Vec<(String, Option<i64>)>, // (uuid, timestamp_ms)
```

既に全行を読んでいる処理に相乗りするだけなので、追加の I/O は発生しない。

### 2. 永続化

新テーブルを追加する（`store.rs`）。

```sql
CREATE TABLE IF NOT EXISTS message_uuid (
    session_id TEXT NOT NULL,
    uuid       TEXT NOT NULL,
    ts_ms      INTEGER,
    PRIMARY KEY (session_id, uuid)
);
CREATE INDEX IF NOT EXISTS idx_message_uuid_uuid ON message_uuid(uuid);
```

- mtime/size キャッシュが有効なセッションは再走査されないため、このテーブルへの書き込みも発生しない（既存のキャッシュ戦略を踏襲）
- 再走査が起きたセッションは `DELETE FROM message_uuid WHERE session_id = ?` してから全 uuid を再挿入する
- 1 セッションあたり数千〜数万行になり得るが、SQLite の PK 制約とインデックスで実用上問題ない規模（実データの最大セッションで uuid 数 2 万件程度）

### 3. Fork ペアの検出

起動時に 1 クエリで「複数セッションに出現する uuid」を引く。

```sql
SELECT uuid, session_id, ts_ms
FROM message_uuid
WHERE uuid IN (
    SELECT uuid FROM message_uuid GROUP BY uuid HAVING COUNT(DISTINCT session_id) > 1
)
```

Rust 側でこの結果から `(session_id, session_id) -> 共有uuid数` の隣接情報を作り、Union-Find で連結成分（= fork グループ）にまとめる。

### 4. 起源の決定

各グループについて、メンバーの `SessionRow.created`（birthtime ベース、既存フィールドを流用）が最も古いものを起源とする。同値の場合は `session_id` の辞書順などタイブレークルールを決めておく（発生頻度は低いはずなので厳密さは求めない）。

### 5. 新モジュール

`src/fork.rs` を新設し、上記の Union-Find・起源判定をここに閉じ込める（`filter.rs` や `grep.rs` と同格の単機能モジュール）。

```rust
pub struct ForkGroup {
    pub root: String,          // 起源 session_id
    pub members: Vec<String>,  // root を含む全メンバー (session_id)
}

pub fn detect_fork_groups(pairs: &[(String, String)], rows: &[SessionRow]) -> Vec<ForkGroup>;
```

## SessionRow への反映

`rows::build` の中で `detect_fork_groups` の結果を引いて、各行に付随情報を持たせる。

```rust
// SessionRow に追加
pub fork: Option<ForkMark>,

pub struct ForkMark {
    pub is_root: bool,
    pub group_members: Vec<String>, // 自分以外の同グループ session_id (表示用)
}
```

## UI 設計

### 一覧

既存の `RUNNING_MARK = "●"` / `CWD_MISSING_MARK = "✗"` と同じ記号ベースの流儀に合わせる（絵文字は使わない）。

- 起源セッション: ID 列の先頭に `◆` を付ける
- fork 由来の非起源セッション: ID 列の先頭に `⑂`（分岐）を付ける
- 関係のないセッション: 何も付けない（既存表示のまま）

### 詳細ペイン（画面下部）

選択中セッションが fork グループに属する場合、1 行追加する。

```
fork: 起源 (このセッションが起源。他に2件の派生セッションあり)
fork: 派生元は aaaa1111 (WAFボット対策) 2026-07-16 開始。他に1件の派生セッションあり
```

派生セッションの一覧を全部出すと detail 欄が膨らむので、まずは「件数 + 起源への導線」に留める。全件見たい場合の一覧化（例: 専用モード）は今回のスコープ外とし、必要になったら別途要望を聞く。

## パフォーマンス影響

- 走査時のオーバーヘッド: 既存ループへの相乗りのみなので実質ゼロ
- 永続化: 再走査が起きたセッション分だけ `message_uuid` へ書き込み。初回フルスキャン時は全セッション分書き込みが発生するが、2 回目以降はキャッシュヒットで発生しない
- 起動時の fork 検出クエリ: 全体で数十万〜数百万行規模でも SQLite の GROUP BY + インデックスなら数百ms以内で収まる想定（実データ 102 セッション・十数万行の実測は未実施なので、実装後に確認する）

## 実装ステップ（設計 → テストコード → 実装の順で進める）

1. `session.rs`: `scan_jsonl` に uuid+timestamp 収集を追加、既存テストに影響が出ないことを確認
2. `store.rs`: `message_uuid` テーブルの migrate、save/load 関数、ユニットテスト
3. `fork.rs` 新設: Union-Find + 起源判定のロジックとユニットテスト（実データ相当のケース: 単純 fork / 3 方向 fork / fork 無し / compact で共有 uuid が少ないケース）
4. `rows.rs`: `SessionRow` に `fork` フィールドを追加、`build` から `detect_fork_groups` を呼ぶ配線
5. `tui/ui.rs`: 一覧マーク・詳細ペイン表示の追加とテスト
6. 実データでの動作確認（`--ignored` の real_data_smoke 相当、または手動確認）

## 未決事項・スコープ外

- 派生セッションの全件ツリー表示（今回はフラットな件数表示のみ）
- 「どれが直接の親か」の厳密な推定（compact・多方向分岐があるため今回は行わない）
- サブエージェント記録（`isSidechain: true`）は元々一覧から除外されているため、fork 検出の対象は本体セッションのみで良い

## 実装時に判明した差分（設計からの変更点）

- **`rows::build` のシグネチャは変えず、`apply_fork_marks` を別関数として追加した**。fork 検出には全セッションの message uuid が DB に出揃っている必要があるが、それは `build` より後 (loader 側で fork 検出を実行した後) にしか判定できない。`build` のシグネチャに引数を足すと既存の呼び出し 9 箇所 (テスト含む) を全部触ることになるため、`build` は今まで通り `fork: None` で行を作り、`loader::load_with` が `apply_fork_marks` で後から反映する 2 段階にした。
- **既存の `session_cache` に対するマイグレーションが必要だった**。`scan_all` は `(mtime, size)` が変わっていないファイルを再走査しないため、`message_uuid` テーブルを新設しただけでは、既存ユーザーの `session_cache` がキャッシュヒットし続ける限り `message_uuid` に一切データが入らず、fork 検出が永久に機能しない。`jsonl_timestamp_ms` 列追加時 (機能3) と同じ理由・同じパターンで、`message_uuid` テーブルを新設したタイミングの初回起動時に一度だけ `session_cache` を全件破棄し、次回起動時に再走査させるマイグレーションを `store.rs` に追加した。
- 軽量文字列抽出 (JSON フルパースをしない `extract_str_field`) を採用した。実測 (984MB・約27万行) でフルパースの約半分の時間で済み、既存の `scan_jsonl` の early-exit 最適化を壊さずに済むため。
- 実データ (102 セッション) で確認: 24 組の fork ペア・34 セッションが何らかの fork グループに属する。3 方向以上の分岐 (例: `062cdb26`/`f6da54c8`/`9ef3bb4b`) も正しく 1 グループにまとまることを確認済み。
