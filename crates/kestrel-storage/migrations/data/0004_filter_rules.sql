-- Filter rules for automated mail processing (ADR 0004: durable records in
-- data.db; rules survive restarts).

CREATE TABLE filter_rules (
    id               TEXT NOT NULL PRIMARY KEY,        -- rule UUID
    account_id       TEXT NOT NULL,
    name             TEXT NOT NULL,
    enabled          INTEGER NOT NULL DEFAULT 1,
    priority         INTEGER NOT NULL DEFAULT 0,
    condition_logic  TEXT NOT NULL DEFAULT 'and',      -- 'and' | 'or'
    created_at       INTEGER NOT NULL,                 -- unix ms
    updated_at       INTEGER NOT NULL
);
CREATE INDEX idx_filter_rules_account ON filter_rules(account_id);

CREATE TABLE filter_conditions (
    id          TEXT NOT NULL PRIMARY KEY,             -- condition UUID
    rule_id     TEXT NOT NULL REFERENCES filter_rules(id) ON DELETE CASCADE,
    field       TEXT NOT NULL,                         -- from|to|cc|subject|body|header|has_attachment
    operator    TEXT NOT NULL,                         -- contains|equals|matches|regex|exists
    value       TEXT NOT NULL,
    negate      INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX idx_filter_conditions_rule ON filter_conditions(rule_id);

CREATE TABLE filter_actions (
    id           TEXT NOT NULL PRIMARY KEY,            -- action UUID
    rule_id      TEXT NOT NULL REFERENCES filter_rules(id) ON DELETE CASCADE,
    action_type  TEXT NOT NULL,                        -- move_to|copy_to|flag|mark_read|delete|forward
    action_param TEXT NOT NULL                         -- JSON param (FolderId, Flag list, email addr)
);
CREATE INDEX idx_filter_actions_rule ON filter_actions(rule_id);
