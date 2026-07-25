## 背景

`~/.claude/projects/` 配下に61プロジェクト・3672セッション(3.1GB)が溜まっており、どのセッションが何をやっていたか一覧で追えない。fzf風のインクリメンタルセレクタで一覧・検索・操作できるCUIツールをRustで作る。

既存の `merge-session.py`(セッション統合)・`sync-to-s3.sh`(S3バックアップ)も同一バイナリのサブコマンドとしてRustへ移植し、リポジトリ全体をRust製CLIに統一する。

## データソース

### セッションjsonl

パス: `~/.claude/projects/<encoded-cwd>/<sessionId>.jsonl`(1行1JSON)

- 共通キー: `type`, `sessionId`, `timestamp`(ISO8601 UTC), `cwd`, `version`, `gitBranch`, `uuid`, `parentUuid`
- user/assistant行: `message: {role, content}`
- タイトル:
  - 自動: `{"type":"ai-title","aiTitle":"..."}`
  - `/rename`手動: `{"type":"custom-title","customTitle":"..."}` と `{"type":"agent-name","agentName":"..."}` が同時出力。複数回出ることがあるので最後に出現した値を採用する
- 作成日時: jsonl内最初の行の`timestamp`(ISO8601)を使う。ファイルの`birthtime`はコピー・PC移行等で書き換わり実際のセッション開始日時と乖離するため使わない(実例: 旧Mac→新Mac移行時のコピーで280件のbirthtimeが移行日に一括更新されていた)
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
- タイトル(custom-title優先、無ければai-title、両方無ければ最初の実質的なuserメッセージ冒頭を表示、それも取得できなければ`(無題)`)
  - スラッシュコマンド実行はuserメッセージが`<command-name>.../<command-message>...`という内部表現の生テキストになる。これは実質的な発言とみなさずスキップし、次のuserメッセージを探す
  - 全てのuserメッセージがコマンド実行だけだったセッションは、最後の手段として`<command-name>`の中身(例: `/kibela-reflect`)をタイトルに使う
- 紐づくRedmineチケット番号(タイトル/タスクsubjectから抽出、複数あれば全部)
- タスク数: `pending` / `in_progress` / `done` の内訳
- 作成日時(jsonl内最初の行のtimestamp)
- cwdが現存するかどうかを可視化する(resumeしてから気づくのではなく一覧の時点で分かるようにする)

デフォルトは作成日時降順。

既定では`isSidechain: true`のサブエージェント記録(`<sessionId>/subagents/agent-*.jsonl`)を一覧から除外し、本体セッションのみ表示する。TUI起動中はキー操作でサブエージェント記録の表示/非表示をその場でトグルできる(既存のキーバインドと衝突しないキーを割り当てる)。起動時の初期状態はCLIオプション`--include-subagents`でも指定できる。

### 操作

- **resume起動**: 選択したセッションのcwdへ`cd`した上で`claude --resume <sessionId>`を起動する(cwdが現存しない場合はエラー表示)。claude終了(`/exit`等)後はcst自体を終了せず一覧に戻る(該当セッションのjsonlだけ再走査され、他はキャッシュが効く)
- **cwd一時上書き**: cwdが存在しない/変更したいセッションを、専用キーでresume起動時のcwdだけその場で上書きしてから起動する。jsonl本体は書き換えない(永続的なパス一括置換は対象外、必要になったら別のバッチ操作として設計する)
- **削除**: 対象jsonl・対応する`~/.claude/tasks/<sessionId>/`を削除する。実行中(セッションレジストリに`status: idle/busy/bg`で存在)のセッションは削除前に警告し、確認を挟む
- **アーカイブ**: 別ディレクトリ(`~/.claude/projects-archive/`等)へ移動する(削除より安全な選択肢)
- **内容検索してジャンプ**: キーワードでjsonl全文をgrepし、ヒットしたセッションに絞り込む
- **分類・タグ付け**: ツール側の別ストア(sqlite等)に手動タグを保存する。jsonl自体は編集しない
- **要約・recap**: 選択したセッションに対し `claude --resume <sessionId> -p "<要約prompt>" --model <軽量モデル>` をサブプロセスとして呼び出し、結果を表示する。Rust側でAnthropic API鍵を直接扱わず、既存のclaude CLI認証をそのまま使う
- **ヘルプ表示**: 専用キーでショートカット一覧と各操作の挙動(確認が必要な操作等)を説明するオーバーレイを開閉できる。既存のキーバインドと衝突しないキーを割り当てる

### merge-session.pyの移植(サブコマンド化)

既存Python版の挙動をそのまま踏襲する。

- コマンド: `<bin> merge <session_a.jsonl> <session_b.jsonl> [--append] [--dry-run]`
- セッションBのuser/assistantメッセージを抽出し、UUID/parentUuid/sessionIdを再採番してセッションAへ挿入する
  - デフォルト: セッションAの先頭(最初のuserメッセージの前)に挿入
  - `--append`: セッションAの末尾(最後のuser/assistantメッセージの後)に追加
- `--dry-run`: 書き込まずマージ後の件数・会話プレビュー(先頭10件)のみ表示
- 実行時は上書き前に`<session_a_path>.bak.<YYYYMMDDHHMMSS>`へバックアップする
- 完了後「`claude --resume <session_id>`して`/compact`」の案内を表示する

### sync-to-s3.shの移植(サブコマンド化)

既存Shell版の挙動をそのまま踏襲する。cron実行前提でネットワーク断時も非エラー終了する。

- コマンド: `<bin> sync-s3`
- `AWS_PROFILE=test`でバケット`yotsuya-test`への疎通確認(head-bucket、タイムアウト5秒)。失敗時は何もせず正常終了(exit 0)
- 疎通OKなら`~/.claude/projects/`を`s3://yotsuya-test/claude-sessions/projects/`へsync(`--exclude *.lock`・`--size-only`・エラー時のみ出力)
- AWS認証はaws-sdk-s3 + aws-configで環境変数`AWS_PROFILE`をそのまま利用する(鍵をRust側にハードコードしない)

## 技術選定

- 言語: Rust
- CLI: `clap`(derive)。サブコマンド無し実行でTUI起動、`merge`/`sync-s3`をサブコマンドとして提供
- TUI: `ratatui`
- fuzzy matcher: `nucleo`(helixが採用しているインクリメンタルマッチャー。`skim`ライブラリでも可、実装時に比較して選定)
- jsonl parse: `serde_json`(1行ずつstreaming読み込み。3672ファイル×可変行数のため全文を一度にメモリへ載せない)
- SQLite読み取り: `rusqlite`
- ファイル探索: `walkdir`
- 非同期ランタイム: `tokio`(aws-sdk-s3が非同期APIのため。TUI/merge部分は同期のままでよい)
- S3: `aws-sdk-s3` / `aws-config`

## スコープ

- 対象: `~/.claude/projects/` 配下の全プロジェクト横断
- 既存のtodo運用・スキル(y-find-session、todo自動キャプチャ等)とは完全独立。将来連携したくなったら拡張する
- リポジトリ内の`merge-session.py`・`sync-to-s3.sh`はRust版と同一挙動になったら削除する

## 非対象(今回のスコープ外)

- todo.md/journalとの連携
- タスクの新規作成・編集(閲覧のみ)
