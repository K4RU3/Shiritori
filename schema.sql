-- ルーム情報
CREATE TABLE rooms (
    id INTEGER PRIMARY KEY     -- プログラム側でu64→i64に変換して保存
);

-- ルームメンバー関係（多対多＋双方向リンク）
CREATE TABLE room_members (
    room_id INTEGER NOT NULL REFERENCES rooms(id) ON DELETE CASCADE,
    user_id INTEGER NOT NULL,
    next INTEGER,               -- 次のユーザー（同ルーム内ユーザーid）
    prev INTEGER,               -- 前のユーザー（同ルーム内ユーザーid）
    state TEXT CHECK(state IN ('good', 'bad', 'none')) DEFAULT 'none',
    PRIMARY KEY (room_id, user_id),
    FOREIGN KEY (room_id, next) REFERENCES room_members(room_id, user_id) ON DELETE SET NULL, -- 消すとroom_idをNULLにしようとするので、next/prevをnullにしてから削除すること
    FOREIGN KEY (room_id, prev) REFERENCES room_members(room_id, user_id) ON DELETE SET NULL
);

-- 投票状態管理
CREATE TABLE room_votes (
    room_id INTEGER PRIMARY KEY REFERENCES rooms(id) ON DELETE CASCADE,
    current_user_id INTEGER NOT NULL,
    word TEXT,                     -- 投票中の単語
    updated_at TEXT DEFAULT (datetime('now'))
);

-- 投票時にVote存在チェック
CREATE TRIGGER room_votes_exists_check
BEFORE UPDATE OF state ON room_members
FOR EACH ROW
WHEN NOT EXISTS (
    SELECT 1 FROM room_votes WHERE room_id = NEW.room_id
)
BEGIN
    SELECT RAISE(ABORT, 'Vote record missing for this room');
END;

-- 投票ワードLowercase + 既出チェック
CREATE TRIGGER voteword_already_used_check
BEFORE INSERT ON room_votes
FOR EACH ROW
BEGIN
    SELECT NEW.word = LOWER(NEW.word);
    SELECT
        CASE
            WHEN EXISTS (
                SELECT 1 FROM room_words
                WHERE room_id = NEW.room_id
                  AND word = NEW.word
            )
            THEN RAISE(ABORT, 'Word already used in this room')
        END;
END;

-- 投票時updated_at自動更新
CREATE TRIGGER update_room_votes_timestamp
AFTER UPDATE ON room_votes
FOR EACH ROW
WHEN NEW.updated_at = OLD.updated_at
BEGIN
    UPDATE room_votes
    SET updated_at = datetime('now')
    WHERE rowid = NEW.rowid;
END;

-- 投票変更時メンバー状態リセット
CREATE TRIGGER reset_room_members_state_on_vote_change
AFTER UPDATE ON room_votes
FOR EACH ROW
BEGIN
    UPDATE room_members
    SET state = 'none'
    WHERE room_id = NEW.room_id;
END;

-- 使われた単語リスト（履歴）
CREATE TABLE room_words (
    room_id INTEGER NOT NULL REFERENCES rooms(id) ON DELETE CASCADE,
    word TEXT NOT NULL,
    PRIMARY KEY (room_id, word)
);
