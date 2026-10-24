-- Historical base 1 birth schema from 56358c56877ee1a82fb3afa8f4bab7d14652f75d:src/engagement.rs.
PRAGMA journal_mode=WAL;
                 PRAGMA foreign_keys=ON;
                 PRAGMA synchronous=FULL;
                 CREATE TABLE IF NOT EXISTS metadata(
                   key TEXT PRIMARY KEY, value TEXT NOT NULL
                 ) STRICT;
                 CREATE TABLE IF NOT EXISTS engagements(
                   engagement_id TEXT PRIMARY KEY,
                   workspace_id TEXT NOT NULL,
                   controller_sha256 TEXT NOT NULL,
                   state TEXT NOT NULL,
                   specialist_run_id TEXT,
                   task_id TEXT,
                   result_sha256 TEXT,
                   result_json TEXT,
                   revision INTEGER NOT NULL
                 ) STRICT;
                 CREATE TABLE IF NOT EXISTS operations(
                   operation TEXT NOT NULL,
                   idempotency_key TEXT NOT NULL,
                   request_sha256 TEXT NOT NULL,
                   response_json TEXT NOT NULL,
                   PRIMARY KEY(operation,idempotency_key)
                 ) STRICT;
                 CREATE TABLE IF NOT EXISTS members(
                   specialist_run_id TEXT PRIMARY KEY,
                   engagement_id TEXT NOT NULL UNIQUE,
                   hire_operation_id TEXT NOT NULL UNIQUE,
                   configuration_sha256 TEXT NOT NULL,
                   state TEXT NOT NULL,
                   FOREIGN KEY(engagement_id) REFERENCES engagements(engagement_id)
                 ) STRICT;
                 CREATE TABLE IF NOT EXISTS tasks(
                   task_id TEXT PRIMARY KEY,
                   engagement_id TEXT NOT NULL UNIQUE,
                   specialist_run_id TEXT NOT NULL,
                   objective_sha256 TEXT NOT NULL,
                   state TEXT NOT NULL,
                   result_sha256 TEXT,
                   FOREIGN KEY(engagement_id) REFERENCES engagements(engagement_id),
                   FOREIGN KEY(specialist_run_id) REFERENCES members(specialist_run_id)
                 ) STRICT;
                 CREATE TABLE IF NOT EXISTS artifacts(
                   artifact_id TEXT PRIMARY KEY,
                   engagement_id TEXT NOT NULL UNIQUE,
                   result_sha256 TEXT NOT NULL UNIQUE,
                   canonical_json TEXT NOT NULL,
                   FOREIGN KEY(engagement_id) REFERENCES engagements(engagement_id)
                 ) STRICT;
                 CREATE TABLE IF NOT EXISTS delivery_receipts(
                   sequence INTEGER PRIMARY KEY AUTOINCREMENT,
                   task_id TEXT NOT NULL UNIQUE,
                   artifact_id TEXT NOT NULL,
                   delivered_at_ms INTEGER NOT NULL,
                   FOREIGN KEY(task_id) REFERENCES tasks(task_id),
                   FOREIGN KEY(artifact_id) REFERENCES artifacts(artifact_id)
                 ) STRICT;
                 CREATE TABLE IF NOT EXISTS events(
                   sequence INTEGER PRIMARY KEY AUTOINCREMENT,
                   engagement_id TEXT NOT NULL,
                   kind TEXT NOT NULL,
                   payload_sha256 TEXT NOT NULL,
                   previous_hash TEXT NOT NULL,
                   event_hash TEXT NOT NULL,
                   created_at_ms INTEGER NOT NULL,
                   FOREIGN KEY(engagement_id) REFERENCES engagements(engagement_id)
                 ) STRICT;
INSERT OR IGNORE INTO metadata(key,value) VALUES('schema_version','1');
