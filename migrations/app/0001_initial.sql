-- Global registry and nonsecret settings. The Rust caller owns the transaction.

CREATE TABLE workspace_registry (
    workspace_id TEXT PRIMARY KEY NOT NULL,
    root_path TEXT NOT NULL UNIQUE,
    name TEXT NOT NULL,
    last_opened_at_ms INTEGER,
    availability TEXT NOT NULL DEFAULT 'ONLINE'
        CHECK (availability IN ('ONLINE', 'OFFLINE'))
);

CREATE TABLE app_settings (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
    config_json TEXT NOT NULL DEFAULT '{}',
    config_revision INTEGER NOT NULL DEFAULT 0 CHECK (config_revision >= 0),
    updated_at_ms INTEGER NOT NULL
);
