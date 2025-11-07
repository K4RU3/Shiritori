-- ルーム情報
CREATE TABLE rooms (
    id INTEGER PRIMARY KEY     -- プログラム側でu64→i64に変換して保存
);

-- ユーザー情報
CREATE TABLE users (
    id INTEGER PRIMARY KEY     -- 同上
);

-- ルームメンバー関係（多対多＋双方向リンク）
CREATE TABLE room_members (
    room_id INTEGER NOT NULL REFERENCES rooms(id) ON DELETE CASCADE,
    user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    next INTEGER,               -- 次のユーザー（同ルーム内ユーザーid）
    prev INTEGER,               -- 前のユーザー（同ルーム内ユーザーid）
    PRIMARY KEY (room_id, user_id),
    FOREIGN KEY (room_id, next) REFERENCES room_members(room_id, user_id) ON DELETE SET NULL, -- 消すとroom_idをNULLにしようとするので、next/prevをnullにしてから削除すること
    FOREIGN KEY (room_id, prev) REFERENCES room_members(room_id, user_id) ON DELETE SET NULL
);

-- 投票状態管理
CREATE TABLE room_votes (
    room_id INTEGER PRIMARY KEY REFERENCES rooms(id) ON DELETE CASCADE,
    current_user_id INTEGER NOT NULL REFERENCES users(id),
    word TEXT,                     -- 投票中の単語
    updated_at TEXT DEFAULT (datetime('now'))
);

-- メンバーの投票状態
CREATE TABLE member_votes (
    room_id INTEGER NOT NULL REFERENCES rooms(id) ON DELETE CASCADE,
    user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    state TEXT CHECK(state IN ('good', 'bad', 'none')) DEFAULT 'none',
    PRIMARY KEY (room_id, user_id)
);

-- 投票変更時自動削除
CREATE TRIGGER clear_member_votes_on_room_vote_change
AFTER UPDATE OF current_user_id, word ON room_votes
FOR EACH ROW
WHEN OLD.current_user_id != NEW.current_user_id
   OR OLD.word IS NOT NEW.word
BEGIN
    DELETE FROM member_votes
    WHERE room_id = NEW.room_id;
END;

-- 使われた単語リスト（履歴）
CREATE TABLE room_words (
    room_id INTEGER NOT NULL REFERENCES rooms(id) ON DELETE CASCADE,
    word TEXT NOT NULL,
    PRIMARY KEY (room_id, word)
);
