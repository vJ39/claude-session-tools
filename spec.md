## 背景

`~/.claude/projects/` 配下に61プロジェクト・3672セッション(3.1GB)が溜まっており、どのセッションが何をやっていたか一覧で追えない。fzf風のインクリメンタルセレクタで一覧・検索・操作できるCUIツールをRustで作る。

既存の `merge-session.py`(セッション統合)・`sync-to-s3.sh`(S3バックアップ)とは目的が異なる(あちらはマージ・退避、今回は日常的な一覧・選択・操作)。同一リポジトリに併存させ、将来的な統合は今回のスコープ外とする。

## データソース

### セッションjsonl

パス: `~/.claude/projects/<encoded-cwd>/<sessionId>.jsonl`(1行1JSON)

- 共通キー: `type`, `sessionId`, `timestamp`(ISO8601 UTC), `cwd`, `version`, `gitBranch`, `uuid`, `parentUuid`
- user/assistant行: `message: {role, content}`
- タイトル:
  - 自動: `{"type":"ai-title","aiTitle":"..."}`
  - `/rename`手動: `{"type":"custom-title","customTitle":"..."}` と `{"type":"agent-name","agentName":"..."}` が同時出力。複数回出ることがあるので最後に出現した値を採用する
- 作成日時: ファイルの`birthtime`(mtimeは最終更新時刻になるため不適)
- `cwd`はディレクトリ名(`-Users-work--ghq-chat-slack`等)からの逆算が非可逆(`/`・`.`・`_`が全て`-`化される)なので、必ず各行の`"cwd"`キーを読む

### タスク永続化

パス: `~/.claude/tasks/<sessionId>/<taskId>.json`(1タスク1ファイル)

- キー: `id`, `subject`, `description`, `activeForm`, `status`(`pending`/`in_progress`/`completed`), `blocks[]`, `blockedBy[]`
- ファイル自体にsessionIdは無く、ディレクトリ階層(`<sessionId>/`)でのみ紐付く
- 削除済みタスクの扱い: `deleted`ステータスは即ファイル削除される想定(要実装時に実挙動を確認)

### セッションレジストリ(実行中プロセス)

パス: `~/.claude/sessions/<pid>.json`

- キー: `pid`, `sessionId`, `cwd`, `startedAt`, `name`, `status`(`idle`/`busy`/`bg`), `jobId`
- `name`が`/rename`タイトルと一致する。実行中セッションの検出に使う(実行中のjsonlを誤って削除しないためのガード)

### worklog.db(SQLite)

パス: `~/.claude/worklog.db`

```sql
CREATE TABLE work_log (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  session_id TEXT NOT NULL,
  project TEXT,
  cwd TEXT,
  start_ts INTEGER NOT NULL,
  stop_ts INTEGER NOT NULL,
  elapsed_sec INTEGER NOT NULL,
  prompt TEXT,
  ticket TEXT,
  tag TEXT
);
```

### Redmineチケット番号

専用フィールドは無く、常に文字列埋め込み。タイトル・タスクsubjectの先頭に`[#NNNN]`または`#NNNN `形式で入る運用。`work_log.ticket`は桁数(10000以上)でRedmine番号かTaskCreate連番かを推定している(専用フラグ無し)。今回のツールも同じ推定ルールで抽出する。

## 機能要件

### 一覧表示

1セッション1行、以下の列を表示する:

- session id(先頭8桁で表示、フルIDは詳細表示や操作時に使う)
- タイトル(custom-title優先、無ければai-title、両方無ければ`(無題)`)
- 紐づくRedmineチケット番号(タイトル/タスクsubjectから抽出、複数あれば全部)
- タスク数: `pending` / `in_progress` / `done` の内訳
- 作成日時(jsonlファイルのbirthtime)

デフォルトは作成日時降順。

### 操作

- **resume起動**: 選択したセッションのcwdへ`cd`した上で`claude --resume <sessionId>`を起動する(cwdが現存しない場合はエラー表示)
- **削除**: 対象jsonl・対応する`~/.claude/tasks/<sessionId>/`を削除する。実行中(セッションレジストリに`status: idle/busy/bg`で存在)のセッションは削除前に警告し、確認を挟む
- **アーカイブ**: 別ディレクトリ(`~/.claude/projects-archive/`等)へ移動する(削除より安全な選択肢)
- **内容検索してジャンプ**: キーワードでjsonl全文をgrepし、ヒットしたセッションに絞り込む
- **分類・タグ付け**: ツール側の別ストア(sqlite等)に手動タグを保存する。jsonl自体は編集しない
- **要約・recap**: 選択したセッションに対し `claude --resume <sessionId> -p "<要約prompt>" --model <軽量モデル>` をサブプロセスとして呼び出し、結果を表示する。Rust側でAnthropic API鍵を直接扱わず、既存のclaude CLI認証をそのまま使う

## 技術選定

- 言語: Rust
- TUI: `ratatui`
- fuzzy matcher: `nucleo`(helixが採用しているインクリメンタルマッチャー。`skim`ライブラリでも可、実装時に比較して選定)
- jsonl parse: `serde_json`(1行ずつstreaming読み込み。3672ファイル×可変行数のため全文を一度にメモリへ載せない)
- SQLite読み取り: `rusqlite`
- ファイル探索: `walkdir`

## スコープ

- 対象: `~/.claude/projects/` 配下の全プロジェクト横断
- 既存のtodo運用・スキル(y-find-session、todo自動キャプチャ等)とは完全独立。将来連携したくなったら拡張する

## 非対象(今回のスコープ外)

- merge-session.py/sync-to-s3.shとの統合・Rust化
- todo.md/journalとの連携
- タスクの新規作成・編集(閲覧のみ)
