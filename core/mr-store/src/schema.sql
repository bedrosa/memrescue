-- MemRescue: esquema de la base de datos de trabajo, versión 1.
-- Regla: aquí solo se guardan metadatos, progreso y estado de reanudación.
-- El contenido binario recuperado o volcado se guarda en ficheros independientes.

CREATE TABLE schema_meta (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
) STRICT;

-- Orígenes de datos (imágenes; en fases posteriores, discos y RAID virtuales).
CREATE TABLE sources (
    id          INTEGER PRIMARY KEY,
    kind        TEXT    NOT NULL CHECK (kind IN ('image', 'device', 'raid', 'partition')),
    path        TEXT    NOT NULL,
    size_bytes  INTEGER NOT NULL CHECK (size_bytes >= 0),
    sector_size INTEGER NOT NULL CHECK (sector_size >= 512),
    sha256      TEXT,
    added_at    TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
) STRICT;

-- Trabajos de larga duración (hash, escaneo, recuperación, clonado...).
CREATE TABLE jobs (
    id             INTEGER PRIMARY KEY,
    kind           TEXT    NOT NULL,
    source_id      INTEGER REFERENCES sources (id),
    state          TEXT    NOT NULL CHECK (state IN
                     ('pending', 'running', 'paused', 'interrupted', 'cancelled', 'failed', 'completed')),
    params_json    TEXT    NOT NULL DEFAULT '{}',
    progress_done  INTEGER NOT NULL DEFAULT 0 CHECK (progress_done >= 0),
    progress_total INTEGER NOT NULL DEFAULT 0 CHECK (progress_total >= 0),
    result_json    TEXT,
    error          TEXT,
    created_at     TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    updated_at     TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
) STRICT;

CREATE INDEX jobs_state ON jobs (state);

-- Puntos de control para reanudar: posición + estado pequeño (JSON). Nunca datos recuperados.
CREATE TABLE job_checkpoints (
    job_id     INTEGER NOT NULL REFERENCES jobs (id) ON DELETE CASCADE,
    seq        INTEGER NOT NULL,
    position   INTEGER NOT NULL CHECK (position >= 0),
    state_json TEXT    NOT NULL DEFAULT '{}',
    created_at TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    PRIMARY KEY (job_id, seq)
) STRICT;

-- Registro de eventos de cada trabajo.
CREATE TABLE job_events (
    id      INTEGER PRIMARY KEY,
    job_id  INTEGER NOT NULL REFERENCES jobs (id) ON DELETE CASCADE,
    at      TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    level   TEXT    NOT NULL CHECK (level IN ('info', 'warning', 'error')),
    message TEXT    NOT NULL
) STRICT;
