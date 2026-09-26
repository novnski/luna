-- Migration v14, following device-owned project command storage v13.
CREATE TABLE side_chats (
    seq INTEGER PRIMARY KEY AUTOINCREMENT,
    id TEXT UNIQUE NOT NULL,
    parent_id TEXT NOT NULL,
    title TEXT NOT NULL,
    created_at_ms INTEGER NOT NULL,
    expires_at_ms INTEGER NOT NULL,
    state TEXT NOT NULL,
    detail TEXT,
    config TEXT NOT NULL,
    cwd TEXT NOT NULL,
    context TEXT NOT NULL,
    context_mode TEXT NOT NULL,
    origin_message_id TEXT,
    anchor_text TEXT,
    creation_hash TEXT NOT NULL,
    needs_recovery INTEGER NOT NULL DEFAULT 0
) STRICT;
CREATE INDEX side_chats_parent ON side_chats(parent_id,seq DESC);
CREATE TABLE side_chat_entries (
    chat_id TEXT NOT NULL,
    seq INTEGER NOT NULL,
    entry TEXT NOT NULL,
    PRIMARY KEY(chat_id,seq)
) STRICT;
CREATE TABLE side_chat_sends (
    chat_id TEXT NOT NULL,
    message_id TEXT NOT NULL,
    state TEXT NOT NULL,
    payload_hash TEXT NOT NULL,
    PRIMARY KEY(chat_id,message_id)
) STRICT;
