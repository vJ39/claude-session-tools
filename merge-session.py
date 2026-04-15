#!/usr/bin/env python3
"""
Claude Code セッションマージツール
セッションBの会話をセッションAに取り込む

使い方:
  python3 merge-session.py <session_a_jsonl> <session_b_jsonl> [--append] [--dry-run]

セッションBのuser/assistantメッセージをセッションAに挿入する。
  デフォルト: Aの先頭（最初のuserメッセージの前）に挿入
  --append:   Aの末尾に追加

その後 claude --resume <session_a_id> して /compact すれば圧縮される。
"""

import json
import sys
import uuid
import shutil
from datetime import datetime
from pathlib import Path


def load_jsonl(path):
    lines = []
    with open(path) as f:
        for line in f:
            line = line.strip()
            if line:
                lines.append(json.loads(line))
    return lines


def extract_conversation(entries):
    """user/assistantメッセージだけ抽出"""
    return [e for e in entries if e.get("type") in ("user", "assistant")]


def find_first_user_index(entries):
    """セッションAの最初のuserメッセージのインデックスを返す"""
    for i, e in enumerate(entries):
        if e.get("type") == "user":
            return i
    return len(entries)


def rewrite_messages(messages, session_id, parent_uuid):
    """
    メッセージのUUID, sessionId, parentUuidを書き換える。
    返り値: (書き換え後メッセージリスト, 最後のuuid)
    """
    rewritten = []
    prev_uuid = parent_uuid

    for msg in messages:
        new_uuid = str(uuid.uuid4())
        new_msg = dict(msg)
        new_msg["uuid"] = new_uuid
        new_msg["parentUuid"] = prev_uuid
        new_msg["sessionId"] = session_id
        new_msg["isSidechain"] = False

        # requestIdも新規生成（assistant）
        if new_msg.get("type") == "assistant" and "requestId" in new_msg:
            new_msg["requestId"] = f"req_merged_{uuid.uuid4().hex[:24]}"

        prev_uuid = new_uuid
        rewritten.append(new_msg)

    return rewritten, prev_uuid


def find_last_message_uuid(entries):
    """セッションの最後のuser/assistantメッセージのuuidを返す"""
    for e in reversed(entries):
        if e.get("type") in ("user", "assistant"):
            return e.get("uuid")
    return None


def merge(session_a_path, session_b_path, dry_run=False, append=False):
    a_entries = load_jsonl(session_a_path)
    b_entries = load_jsonl(session_b_path)

    # セッションAのsessionIdを取得
    session_id = None
    for e in a_entries:
        if e.get("sessionId"):
            session_id = e["sessionId"]
            break

    if not session_id:
        print("エラー: セッションAからsessionIdが見つからない")
        sys.exit(1)

    # セッションBから会話を抽出
    b_conversation = extract_conversation(b_entries)
    if not b_conversation:
        print("エラー: セッションBに会話メッセージがない")
        sys.exit(1)

    mode = "末尾に追加" if append else "先頭に挿入"

    if append:
        # 末尾追加モード: Aの最後のメッセージの後にBを繋ぐ
        last_uuid = find_last_message_uuid(a_entries)
        if not last_uuid:
            print("エラー: セッションAにメッセージがない")
            sys.exit(1)

        rewritten_b, _ = rewrite_messages(b_conversation, session_id, last_uuid)
        merged = a_entries + rewritten_b
    else:
        # 先頭挿入モード（従来の動作）
        first_user_idx = find_first_user_index(a_entries)
        if first_user_idx == 0:
            print("エラー: セッションAにヘッダーがない")
            sys.exit(1)

        first_user = a_entries[first_user_idx]
        original_parent = first_user.get("parentUuid")

        rewritten_b, last_b_uuid = rewrite_messages(
            b_conversation, session_id, original_parent
        )

        a_entries[first_user_idx] = dict(a_entries[first_user_idx])
        a_entries[first_user_idx]["parentUuid"] = last_b_uuid

        header = a_entries[:first_user_idx]
        body = a_entries[first_user_idx:]
        merged = header + rewritten_b + body

    if dry_run:
        print(f"モード: {mode}")
        print(f"セッションA: {session_a_path}")
        print(f"  メッセージ数: {len(a_entries)}")
        print(f"  sessionId: {session_id}")
        print(f"セッションB: {session_b_path}")
        print(f"  会話メッセージ数: {len(b_conversation)}")
        print(f"マージ後: {len(merged)} エントリ")
        print()
        print("--- セッションBの会話プレビュー ---")
        for msg in b_conversation[:10]:
            role = msg.get("type", "?")
            content = msg.get("message", {}).get("content", "")
            if isinstance(content, list):
                text = ""
                for block in content:
                    if isinstance(block, dict) and block.get("type") == "text":
                        text += block["text"]
                    elif isinstance(block, dict) and block.get("type") == "thinking":
                        text += "[thinking...]"
                content = text
            preview = str(content)[:80]
            print(f"  [{role}] {preview}")
        if len(b_conversation) > 10:
            print(f"  ... 他 {len(b_conversation) - 10} メッセージ")
    else:
        # バックアップ
        backup_path = f"{session_a_path}.bak.{datetime.now().strftime('%Y%m%d%H%M%S')}"
        shutil.copy2(session_a_path, backup_path)
        print(f"バックアップ: {backup_path}")

        # 書き込み
        with open(session_a_path, "w") as f:
            for entry in merged:
                f.write(json.dumps(entry, ensure_ascii=False) + "\n")

        print(f"マージ完了: {len(rewritten_b)} メッセージを{mode}")
        print(f"次のステップ: claude --resume {session_id} して /compact")


if __name__ == "__main__":
    if len(sys.argv) < 3:
        print("使い方: python3 merge-session.py <session_a.jsonl> <session_b.jsonl> [--append] [--dry-run]")
        sys.exit(1)

    a_path = sys.argv[1]
    b_path = sys.argv[2]
    dry_run = "--dry-run" in sys.argv
    append = "--append" in sys.argv

    if not Path(a_path).exists():
        print(f"エラー: {a_path} が見つからない")
        sys.exit(1)
    if not Path(b_path).exists():
        print(f"エラー: {b_path} が見つからない")
        sys.exit(1)

    merge(a_path, b_path, dry_run, append)
