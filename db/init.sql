-- API Keys main table
CREATE TABLE api_keys (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    key_hash TEXT NOT NULL UNIQUE,  -- SHA-256 hash of the actual key
    name TEXT NOT NULL,              -- Human-readable identifier
    description TEXT,                 -- Optional description
    
    -- Rate limiting configuration
    rate_limit_max_tokens INTEGER NOT NULL DEFAULT 100,
    rate_limit_refill_rate INTEGER NOT NULL DEFAULT 10,  -- tokens per second
    
    -- Quotas
    daily_request_limit INTEGER,     -- NULL means unlimited
    daily_requests_used INTEGER NOT NULL DEFAULT 0,
    quota_reset_at TIMESTAMP NOT NULL,
    
    -- Metadata
    created_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
    last_used_at TIMESTAMP,
    is_active BOOLEAN NOT NULL DEFAULT 1,
    expires_at TIMESTAMP              -- NULL means never expires
);

-- Method permissions table (allowlist approach)
CREATE TABLE api_key_methods (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    api_key_id INTEGER NOT NULL,
    method_name TEXT NOT NULL,
    max_requests_per_day INTEGER,    -- Per-method daily limit
    requests_today INTEGER NOT NULL DEFAULT 0,
    
    FOREIGN KEY (api_key_id) REFERENCES api_keys(id) ON DELETE CASCADE,
    UNIQUE(api_key_id, method_name)
);

-- Usage analytics table
CREATE TABLE api_key_usage (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    api_key_id INTEGER NOT NULL,
    date DATE NOT NULL,
    method_name TEXT NOT NULL,
    request_count INTEGER NOT NULL DEFAULT 0,
    total_latency_ms INTEGER NOT NULL DEFAULT 0,
    error_count INTEGER NOT NULL DEFAULT 0,
    
    FOREIGN KEY (api_key_id) REFERENCES api_keys(id) ON DELETE CASCADE,
    UNIQUE(api_key_id, date, method_name)
);

-- Indexes for performance
CREATE INDEX idx_api_keys_hash ON api_keys(key_hash);
CREATE INDEX idx_api_keys_active ON api_keys(is_active);
CREATE INDEX idx_api_key_methods_lookup ON api_key_methods(api_key_id, method_name);
CREATE INDEX idx_api_key_usage_date ON api_key_usage(api_key_id, date);