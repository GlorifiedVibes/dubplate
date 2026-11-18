/// Persistent library database — tracks + cues/loops.
///
/// Stored at `~/.local/share/dubplate/library.db` (SQLite).
/// Schema mirrors the rekordbox djmdContent / djmdCue tables so a future
/// ANLZ export or rekordbox bridge can read the same data.
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use dubplate_engine::{BeatGrid, Cue, TrackTags};
use rusqlite::{params, Connection};

/// One row of the browser.
#[derive(Clone, Debug)]
pub struct TrackRow {
    pub id: i64,
    pub path: PathBuf,
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub genre: Option<String>,
    pub year: Option<u32>,
    pub bpm: Option<f32>,
    pub bpm_manual: bool,
    /// (tonic pitch class, minor)
    pub key: Option<(u8, bool)>,
    pub duration_secs: Option<f64>,
    /// Waveform preview columns [low, mid, high] (empty until analysed).
    pub preview: Vec<[u8; 3]>,
    pub added_at: i64,
    pub analysed: bool,
}

impl TrackRow {
    /// Tag title, else the file name without its extension.
    pub fn display_title(&self) -> String {
        self.title
            .clone()
            .filter(|t| !t.trim().is_empty())
            .unwrap_or_else(|| self.path.file_stem().and_then(|s| s.to_str()).unwrap_or("?").to_string())
    }
}

/// A hot cue as shown on a browser preview.
#[derive(Clone, Copy, Debug)]
pub struct MiniCue {
    pub time_ms: u32,
    pub out_time_ms: Option<u32>,
    pub color: [u8; 3],
}

pub struct LibraryDb {
    conn: Mutex<Connection>,
}

impl LibraryDb {
    pub fn open() -> Result<Self, rusqlite::Error> {
        let path = db_path();
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).ok();
        }
        let conn = Connection::open(&path)?;
        conn.execute_batch(SCHEMA)?;
        migrate(&conn);
        refresh_stale_analysis(&conn, dubplate_engine::cache::CACHE_VERSION as u32 * 100 + PREVIEW_FORMAT);
        Ok(Self { conn: Mutex::new(conn) })
    }

    /// Records what analysis learned about a track that was just loaded onto a deck.
    /// Never touches tag fields (title/artist/genre).
    pub fn upsert_track(&self, audio_path: &Path, duration_secs: f64, bpm: f32) -> Result<i64, rusqlite::Error> {
        let conn = self.conn.lock().unwrap();
        conn.query_row(
            "INSERT INTO tracks (path, duration_secs, bpm, analysed_at, added_at)
             VALUES (?1, ?2, ?3, strftime('%s','now'), strftime('%s','now'))
             ON CONFLICT(path) DO UPDATE SET
               duration_secs = excluded.duration_secs,
               bpm           = excluded.bpm,
               analysed_at   = excluded.analysed_at
             RETURNING id",
            params![audio_path.to_string_lossy(), duration_secs, bpm],
            |row| row.get(0),
        )
    }

    /// Manual beat-grid override for a track (BPM + downbeat position in chunks).
    pub fn save_grid(&self, audio_path: &Path, grid: BeatGrid) -> Result<(), rusqlite::Error> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO beatgrid_overrides (path, bpm, offset_chunks) VALUES (?1, ?2, ?3)
             ON CONFLICT(path) DO UPDATE SET bpm = excluded.bpm, offset_chunks = excluded.offset_chunks",
            params![audio_path.to_string_lossy(), grid.bpm as f64, grid.offset],
        )?;
        Ok(())
    }

    pub fn load_grid(&self, audio_path: &Path) -> Option<BeatGrid> {
        let conn = self.conn.lock().unwrap();
        conn.query_row(
            "SELECT bpm, offset_chunks FROM beatgrid_overrides WHERE path = ?1",
            params![audio_path.to_string_lossy()],
            |row| Ok(BeatGrid { bpm: row.get::<_, f64>(0)? as f32, offset: row.get(1)? }),
        )
        .ok()
        .filter(|g| g.is_valid())
    }

    pub fn clear_grid(&self, audio_path: &Path) -> Result<(), rusqlite::Error> {
        let conn = self.conn.lock().unwrap();
        conn.execute("DELETE FROM beatgrid_overrides WHERE path = ?1", params![audio_path.to_string_lossy()])?;
        Ok(())
    }

    /// Saves a hot cue / hot loop. A pad holds one cue, so any previous entry for the
    /// same pad is replaced.
    pub fn save_cue(&self, audio_path: &Path, cue: &Cue) -> Result<(), rusqlite::Error> {
        let conn = self.conn.lock().unwrap();
        let track_id: i64 = conn.query_row(
            "INSERT INTO tracks (path, analysed_at)
             VALUES (?1, strftime('%s','now'))
             ON CONFLICT(path) DO UPDATE SET path = excluded.path
             RETURNING id",
            params![audio_path.to_string_lossy()],
            |row| row.get(0),
        )?;
        if let Some(pad) = cue.hot_cue {
            conn.execute("DELETE FROM cues WHERE track_id = ?1 AND hot_cue = ?2", params![track_id, pad as i64])?;
        }
        conn.execute(
            "INSERT INTO cues
               (track_id, time_ms, out_time_ms, hot_cue, label, color_r, color_g, color_b,
                pos_frames, out_frames)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                track_id,
                cue.time_ms,
                cue.out_time_ms,
                cue.hot_cue.map(|n| n as i64),
                &cue.label,
                cue.color[0] as i64,
                cue.color[1] as i64,
                cue.color[2] as i64,
                cue.pos_frames,
                cue.out_frames,
            ],
        )?;
        Ok(())
    }

    /// Load all hot cues/loops for a track (rows with exact frame positions).
    pub fn load_cues(&self, audio_path: &Path) -> Vec<Cue> {
        let conn = self.conn.lock().unwrap();
        let Ok(mut stmt) = conn.prepare(
            "SELECT c.time_ms, c.out_time_ms, c.hot_cue, c.label,
                    c.color_r, c.color_g, c.color_b, c.pos_frames, c.out_frames
             FROM cues c
             JOIN tracks t ON t.id = c.track_id
             WHERE t.path = ?1 AND c.pos_frames IS NOT NULL
             ORDER BY c.hot_cue, c.time_ms",
        ) else {
            return Vec::new();
        };

        stmt.query_map(params![audio_path.to_string_lossy()], |row| {
            Ok(Cue {
                time_ms: row.get(0)?,
                out_time_ms: row.get(1)?,
                hot_cue: row.get::<_, Option<i64>>(2)?.map(|n| n as u8),
                label: row.get(3)?,
                color: [row.get::<_, i64>(4)? as u8, row.get::<_, i64>(5)? as u8, row.get::<_, i64>(6)? as u8],
                pos_frames: row.get(7)?,
                out_frames: row.get(8)?,
            })
        })
        .map(|rows| rows.flatten().collect())
        .unwrap_or_default()
    }

    /// The track's main cue point (where CUE returns to / the track loads), in source frames.
    pub fn save_main_cue(&self, audio_path: &Path, pos_frames: f64) -> Result<(), rusqlite::Error> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO main_cues (path, pos_frames) VALUES (?1, ?2)
             ON CONFLICT(path) DO UPDATE SET pos_frames = excluded.pos_frames",
            params![audio_path.to_string_lossy(), pos_frames],
        )?;
        Ok(())
    }

    pub fn load_main_cue(&self, audio_path: &Path) -> Option<f64> {
        let conn = self.conn.lock().unwrap();
        conn.query_row(
            "SELECT pos_frames FROM main_cues WHERE path = ?1",
            params![audio_path.to_string_lossy()],
            |row| row.get(0),
        )
        .ok()
    }

    /// The drops the user put in by hand for a track (seconds), which take the place of the ones analysis finds.
    /// `None` forgets them. An empty list is kept: it says the track has no drop.
    pub fn save_drops(&self, audio_path: &Path, drops: Option<&[f64]>) -> Result<(), rusqlite::Error> {
        let conn = self.conn.lock().unwrap();
        let path = audio_path.to_string_lossy();
        match drops {
            None => conn.execute("DELETE FROM drop_edits WHERE path = ?1", params![path])?,
            Some(d) => {
                let text = d.iter().map(|t| format!("{t:.4}")).collect::<Vec<_>>().join(",");
                conn.execute(
                    "INSERT INTO drop_edits (path, drops) VALUES (?1, ?2)
                     ON CONFLICT(path) DO UPDATE SET drops = excluded.drops",
                    params![path, text],
                )?
            }
        };
        Ok(())
    }

    pub fn load_drops(&self, audio_path: &Path) -> Option<Vec<f64>> {
        let conn = self.conn.lock().unwrap();
        let text: String = conn
            .query_row("SELECT drops FROM drop_edits WHERE path = ?1", params![audio_path.to_string_lossy()], |row| {
                row.get(0)
            })
            .ok()?;
        Some(text.split(',').filter_map(|t| t.trim().parse().ok()).collect())
    }

    // ── Library index ─────────────────────────────────────────────────────────

    pub fn sources(&self) -> Vec<PathBuf> {
        let conn = self.conn.lock().unwrap();
        let Ok(mut stmt) = conn.prepare("SELECT path FROM sources ORDER BY path") else { return Vec::new() };
        stmt.query_map([], |r| r.get::<_, String>(0))
            .map(|rows| rows.flatten().map(PathBuf::from).collect())
            .unwrap_or_default()
    }

    pub fn add_source(&self, dir: &Path) -> Result<(), rusqlite::Error> {
        let conn = self.conn.lock().unwrap();
        conn.execute("INSERT OR IGNORE INTO sources (path) VALUES (?1)", params![dir.to_string_lossy()])?;
        Ok(())
    }

    pub fn remove_source(&self, dir: &Path) -> Result<(), rusqlite::Error> {
        let conn = self.conn.lock().unwrap();
        conn.execute("DELETE FROM sources WHERE path = ?1", params![dir.to_string_lossy()])?;
        Ok(())
    }

    /// Adds a file to the index (or refreshes its tags). A changed file is queued for
    /// re-analysis. Returns the track id.
    pub fn index_file(&self, path: &Path, tags: &TrackTags, mtime: i64, size: i64) -> Result<i64, rusqlite::Error> {
        let conn = self.conn.lock().unwrap();
        conn.query_row(
            "INSERT INTO tracks (path, title, artist, album, genre, year, added_at, mtime, size, analysed)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, strftime('%s','now'), ?7, ?8, 0)
             ON CONFLICT(path) DO UPDATE SET
               title    = excluded.title,
               artist   = excluded.artist,
               album    = excluded.album,
               genre    = excluded.genre,
               year     = excluded.year,
               analysed = CASE WHEN tracks.mtime IS NOT excluded.mtime THEN 0 ELSE tracks.analysed END,
               mtime    = excluded.mtime,
               size     = excluded.size
             RETURNING id",
            params![
                path.to_string_lossy(),
                tags.title,
                tags.artist,
                tags.album,
                tags.genre,
                tags.year.map(|y| y as i64),
                mtime,
                size
            ],
            |row| row.get(0),
        )
    }

    /// Stores analysis results for the browser (tempo, key, duration, waveform preview).
    /// The tempo the library last stored for a track (its analysis, not a grid you saved).
    pub fn track_bpm(&self, path: &Path) -> Option<f32> {
        let conn = self.conn.lock().unwrap();
        conn.query_row("SELECT bpm FROM tracks WHERE path = ?1", params![path.to_string_lossy()], |r| {
            r.get::<_, Option<f64>>(0)
        })
        .ok()
        .flatten()
        .map(|b| b as f32)
        .filter(|b| *b > 0.0)
    }

    pub fn save_analysis(
        &self,
        path: &Path,
        bpm: f32,
        duration_secs: f64,
        key: Option<(u8, bool, f32)>,
        preview: &[[u8; 3]],
    ) -> Result<(), rusqlite::Error> {
        let blob: Vec<u8> = preview.iter().flat_map(|c| c.iter().copied()).collect();
        let (tonic, minor, conf) = match key {
            Some((t, m, c)) => (Some(t as i64), Some(m as i64), Some(c as f64)),
            None => (None, None, None),
        };
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE tracks SET bpm = ?2, duration_secs = ?3, key_tonic = ?4, key_minor = ?5,
                               key_conf = ?6, preview = ?7, analysed = 1
             WHERE path = ?1",
            params![path.to_string_lossy(), bpm as f64, duration_secs, tonic, minor, conf, blob],
        )?;
        Ok(())
    }

    /// `path -> (mtime, size)` of everything already indexed, so a rescan only reads
    /// tags for new or changed files.
    pub fn known_files(&self) -> HashMap<PathBuf, (i64, i64)> {
        let conn = self.conn.lock().unwrap();
        let mut map = HashMap::new();
        let Ok(mut stmt) = conn.prepare("SELECT path, COALESCE(mtime, -1), COALESCE(size, -1) FROM tracks") else {
            return map;
        };
        if let Ok(rows) = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?, r.get::<_, i64>(2)?)))
        {
            for (p, m, sz) in rows.flatten() {
                map.insert(PathBuf::from(p), (m, sz));
            }
        }
        map
    }

    /// A small value kept with the library (`meta` table).
    pub fn meta_get(&self, key: &str) -> Option<String> {
        let conn = self.conn.lock().unwrap();
        conn.query_row("SELECT value FROM meta WHERE key = ?1", params![key], |r| r.get(0)).ok()
    }

    pub fn meta_set(&self, key: &str, value: &str) {
        let conn = self.conn.lock().unwrap();
        let _ = conn.execute("INSERT OR REPLACE INTO meta (key, value) VALUES (?1, ?2)", params![key, value]);
    }

    pub fn pending_analysis(&self) -> Vec<PathBuf> {
        let conn = self.conn.lock().unwrap();
        let Ok(mut stmt) = conn.prepare("SELECT path FROM tracks WHERE analysed = 0 ORDER BY added_at, path") else {
            return Vec::new();
        };
        stmt.query_map([], |r| r.get::<_, String>(0))
            .map(|rows| rows.flatten().map(PathBuf::from).collect())
            .unwrap_or_default()
    }

    /// Every indexed track. A manual BPM override wins over the analysed tempo.
    pub fn all_tracks(&self) -> Vec<TrackRow> {
        let conn = self.conn.lock().unwrap();
        let Ok(mut stmt) = conn.prepare(
            "SELECT t.id, t.path, t.title, t.artist, t.album, t.genre, t.year,
                    COALESCE(g.bpm, t.bpm), g.bpm IS NOT NULL,
                    t.key_tonic, t.key_minor, t.duration_secs, t.preview,
                    COALESCE(t.added_at, 0), t.analysed
             FROM tracks t LEFT JOIN beatgrid_overrides g ON g.path = t.path",
        ) else {
            return Vec::new();
        };
        stmt.query_map([], |r| {
            let blob: Option<Vec<u8>> = r.get(12)?;
            let preview = blob.map(|b| b.chunks_exact(3).map(|c| [c[0], c[1], c[2]]).collect()).unwrap_or_default();
            let tonic: Option<i64> = r.get(9)?;
            let minor: Option<i64> = r.get(10)?;
            Ok(TrackRow {
                id: r.get(0)?,
                path: PathBuf::from(r.get::<_, String>(1)?),
                title: r.get(2)?,
                artist: r.get(3)?,
                album: r.get(4)?,
                genre: r.get(5)?,
                year: r.get::<_, Option<i64>>(6)?.map(|y| y as u32),
                bpm: r.get::<_, Option<f64>>(7)?.map(|b| b as f32).filter(|b| *b > 0.0),
                bpm_manual: r.get::<_, i64>(8)? != 0,
                key: tonic.zip(minor).map(|(t, m)| (t as u8, m != 0)),
                duration_secs: r.get(11)?,
                preview,
                added_at: r.get(13)?,
                analysed: r.get::<_, i64>(14)? != 0,
            })
        })
        .map(|rows| rows.flatten().collect())
        .unwrap_or_default()
    }

    /// Hot cues / loops for every track, keyed by path, for the browser preview.
    pub fn mini_cues(&self) -> HashMap<PathBuf, Vec<MiniCue>> {
        let conn = self.conn.lock().unwrap();
        let mut map: HashMap<PathBuf, Vec<MiniCue>> = HashMap::new();
        let Ok(mut stmt) = conn.prepare(
            "SELECT t.path, c.hot_cue, c.time_ms, c.out_time_ms, c.color_r, c.color_g, c.color_b
             FROM cues c JOIN tracks t ON t.id = c.track_id
             WHERE c.hot_cue IS NOT NULL ORDER BY c.hot_cue",
        ) else {
            return map;
        };
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                MiniCue {
                    time_ms: r.get(2)?,
                    out_time_ms: r.get(3)?,
                    color: [r.get::<_, i64>(4)? as u8, r.get::<_, i64>(5)? as u8, r.get::<_, i64>(6)? as u8],
                },
            ))
        });
        if let Ok(rows) = rows {
            for (p, c) in rows.flatten() {
                map.entry(PathBuf::from(p)).or_default().push(c);
            }
        }
        map
    }

    // ── Reels (sets of tracks; a track can be on any number of reels) ─────────

    pub fn create_reel(&self, name: &str) -> Result<i64, rusqlite::Error> {
        let name = name.trim();
        let conn = self.conn.lock().unwrap();
        conn.execute("INSERT OR IGNORE INTO reels (name, created) VALUES (?1, strftime('%s','now'))", params![name])?;
        conn.query_row("SELECT id FROM reels WHERE name = ?1", params![name], |r| r.get(0))
    }

    pub fn rename_reel(&self, id: i64, name: &str) -> Result<(), rusqlite::Error> {
        let conn = self.conn.lock().unwrap();
        conn.execute("UPDATE OR IGNORE reels SET name = ?2 WHERE id = ?1", params![id, name.trim()])?;
        Ok(())
    }

    pub fn delete_reel(&self, id: i64) -> Result<(), rusqlite::Error> {
        let conn = self.conn.lock().unwrap();
        conn.execute("DELETE FROM reels WHERE id = ?1", params![id])?;
        Ok(())
    }

    /// `(id, name, track count)` ordered by name.
    pub fn reels(&self) -> Vec<(i64, String, usize)> {
        let conn = self.conn.lock().unwrap();
        let Ok(mut stmt) = conn.prepare(
            "SELECT r.id, r.name, COUNT(rt.track_id) FROM reels r
             LEFT JOIN reel_tracks rt ON rt.reel_id = r.id
             GROUP BY r.id ORDER BY r.name COLLATE NOCASE",
        ) else {
            return Vec::new();
        };
        stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get::<_, i64>(2)? as usize)))
            .map(|rows| rows.flatten().collect())
            .unwrap_or_default()
    }

    /// Adds a track (creating its library row if needed) to the end of a reel.
    pub fn add_to_reel(&self, reel_id: i64, path: &Path) -> Result<(), rusqlite::Error> {
        let conn = self.conn.lock().unwrap();
        let track_id: i64 = conn.query_row(
            "INSERT INTO tracks (path, added_at, analysed) VALUES (?1, strftime('%s','now'), 0)
             ON CONFLICT(path) DO UPDATE SET path = excluded.path RETURNING id",
            params![path.to_string_lossy()],
            |r| r.get(0),
        )?;
        conn.execute(
            "INSERT OR IGNORE INTO reel_tracks (reel_id, track_id, position, added_at)
             VALUES (?1, ?2, COALESCE((SELECT MAX(position) + 1 FROM reel_tracks WHERE reel_id = ?1), 0),
                     strftime('%s','now'))",
            params![reel_id, track_id],
        )?;
        Ok(())
    }

    pub fn remove_from_reel(&self, reel_id: i64, path: &Path) -> Result<(), rusqlite::Error> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "DELETE FROM reel_tracks WHERE reel_id = ?1
               AND track_id = (SELECT id FROM tracks WHERE path = ?2)",
            params![reel_id, path.to_string_lossy()],
        )?;
        Ok(())
    }

    /// Track paths of a reel in play order.
    pub fn reel_paths(&self, reel_id: i64) -> Vec<PathBuf> {
        let conn = self.conn.lock().unwrap();
        let Ok(mut stmt) = conn.prepare(
            "SELECT t.path FROM reel_tracks rt JOIN tracks t ON t.id = rt.track_id
             WHERE rt.reel_id = ?1 ORDER BY rt.position",
        ) else {
            return Vec::new();
        };
        stmt.query_map(params![reel_id], |r| r.get::<_, String>(0))
            .map(|rows| rows.flatten().map(PathBuf::from).collect())
            .unwrap_or_default()
    }

    /// Reel ids a track already belongs to.
    pub fn reels_of(&self, path: &Path) -> Vec<i64> {
        let conn = self.conn.lock().unwrap();
        let Ok(mut stmt) =
            conn.prepare("SELECT rt.reel_id FROM reel_tracks rt JOIN tracks t ON t.id = rt.track_id WHERE t.path = ?1")
        else {
            return Vec::new();
        };
        stmt.query_map(params![path.to_string_lossy()], |r| r.get::<_, i64>(0))
            .map(|rows| rows.flatten().collect())
            .unwrap_or_default()
    }

    /// Delete a cue by hot-cue number (for hot-cue clear).
    pub fn delete_cue(&self, audio_path: &Path, hot_cue: u8) -> Result<(), rusqlite::Error> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "DELETE FROM cues
             WHERE track_id = (SELECT id FROM tracks WHERE path = ?1)
               AND hot_cue = ?2",
            params![audio_path.to_string_lossy(), hot_cue as i64],
        )?;
        Ok(())
    }
}

fn db_path() -> std::path::PathBuf {
    dubplate_engine::paths::data_dir().join("library.db")
}

/// What the stored waveform preview holds: 1 = the old per-chunk bands, 2 = bass, mid and high loudness from the
/// waveform summary. Raised when that changes, so the library redraws every preview.
const PREVIEW_FORMAT: u32 = 2;

/// When the analysis changes (the engine's cache version goes up), tracks analysed by the older one are marked for
/// analysis again, so tempos that an earlier version got wrong are redone. Their stored values stay until then.
fn refresh_stale_analysis(conn: &Connection, version: u32) {
    let _ = conn.execute("CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value TEXT)", []);
    let stored: Option<String> =
        conn.query_row("SELECT value FROM meta WHERE key = 'analysis_version'", [], |r| r.get(0)).ok();
    if stored.as_deref() == Some(version.to_string().as_str()) {
        return;
    }
    // a database with no record yet is either new (nothing to redo) or from before this check (redo)
    let _ = conn.execute("UPDATE tracks SET analysed = 0 WHERE analysed = 1", []);
    let _ = conn.execute(
        "INSERT OR REPLACE INTO meta (key, value) VALUES ('analysis_version', ?1)",
        params![version.to_string()],
    );
}

fn migrate(conn: &Connection) {
    let _ = conn.execute("ALTER TABLE cues ADD COLUMN pos_frames REAL", []);
    let _ = conn.execute("ALTER TABLE cues ADD COLUMN out_frames REAL", []);
    for col in [
        "artist TEXT",
        "album TEXT",
        "genre TEXT",
        "year INTEGER",
        "key_tonic INTEGER",
        "key_minor INTEGER",
        "key_conf REAL",
        "preview BLOB",
        "added_at INTEGER",
        "mtime INTEGER",
        "size INTEGER",
        "analysed INTEGER NOT NULL DEFAULT 0",
    ] {
        let _ = conn.execute(&format!("ALTER TABLE tracks ADD COLUMN {col}"), []);
    }
}

const SCHEMA: &str = "
PRAGMA journal_mode = WAL;
-- with a write-ahead log this is safe against a crash and saves a sync to disk on every small write (a track
-- loaded, a cue set), each of which would otherwise hold up the frame being drawn
PRAGMA synchronous = NORMAL;
PRAGMA foreign_keys = ON;

CREATE TABLE IF NOT EXISTS tracks (
    id            INTEGER PRIMARY KEY,
    path          TEXT    NOT NULL UNIQUE,
    title         TEXT,
    duration_secs REAL,
    bpm           REAL,
    analysed_at   INTEGER
);

CREATE TABLE IF NOT EXISTS cues (
    id          INTEGER PRIMARY KEY,
    track_id    INTEGER NOT NULL REFERENCES tracks(id) ON DELETE CASCADE,
    time_ms     INTEGER NOT NULL,
    out_time_ms INTEGER,           -- NULL for point cue, set for loop
    hot_cue     INTEGER,           -- 1–8 for hot cue pads, NULL for memory cue
    label       TEXT    NOT NULL DEFAULT '',
    color_r     INTEGER NOT NULL DEFAULT 255,
    color_g     INTEGER NOT NULL DEFAULT 165,
    color_b     INTEGER NOT NULL DEFAULT 0,
    UNIQUE(track_id, hot_cue, time_ms)
);

CREATE INDEX IF NOT EXISTS cues_track ON cues(track_id);

-- Folders the library watches.
CREATE TABLE IF NOT EXISTS sources (path TEXT PRIMARY KEY);

-- Reels: named sets of tracks (a track can be on many reels).
CREATE TABLE IF NOT EXISTS reels (
    id      INTEGER PRIMARY KEY,
    name    TEXT NOT NULL UNIQUE COLLATE NOCASE,
    created INTEGER
);
CREATE TABLE IF NOT EXISTS reel_tracks (
    reel_id  INTEGER NOT NULL REFERENCES reels(id) ON DELETE CASCADE,
    track_id INTEGER NOT NULL REFERENCES tracks(id) ON DELETE CASCADE,
    position INTEGER NOT NULL DEFAULT 0,
    added_at INTEGER,
    PRIMARY KEY (reel_id, track_id)
);

-- Main cue point per track (CUE button).
CREATE TABLE IF NOT EXISTS main_cues (
    path       TEXT PRIMARY KEY,
    pos_frames REAL NOT NULL
);

-- User-set tempo / downbeat; wins over the automatic analysis when present.
CREATE TABLE IF NOT EXISTS beatgrid_overrides (
    path          TEXT PRIMARY KEY,
    bpm           REAL NOT NULL,
    offset_chunks REAL NOT NULL
);

-- Drops the user put in by hand (comma-separated seconds); they win over the ones analysis finds.
CREATE TABLE IF NOT EXISTS drop_edits (
    path  TEXT PRIMARY KEY,
    drops TEXT NOT NULL
);
";

#[cfg(test)]
mod tests {
    use super::*;

    fn mem_db() -> LibraryDb {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(SCHEMA).unwrap();
        LibraryDb { conn: Mutex::new(conn) }
    }

    fn mem_db_migrated() -> LibraryDb {
        let db = mem_db();
        migrate(&db.conn.lock().unwrap());
        db
    }

    #[test]
    fn hand_set_drops_are_kept_and_an_empty_list_is_not_the_same_as_none() {
        let db = mem_db();
        let path = Path::new("/music/d.flac");
        assert_eq!(db.load_drops(path), None);
        db.save_drops(path, Some(&[42.42, 126.39])).unwrap();
        assert_eq!(db.load_drops(path), Some(vec![42.42, 126.39]));
        db.save_drops(path, Some(&[])).unwrap();
        assert_eq!(db.load_drops(path), Some(vec![]));
        db.save_drops(path, None).unwrap();
        assert_eq!(db.load_drops(path), None);
    }

    #[test]
    fn hot_cues_and_loops_round_trip_replace_and_delete() {
        let db = mem_db_migrated();
        let path = Path::new("/music/track.ogg");
        assert!(db.load_cues(path).is_empty());

        db.save_cue(path, &Cue::new(Some(1), 44_100.0, None, 44_100, [0, 200, 80])).unwrap();
        db.save_cue(path, &Cue::new(Some(2), 88_200.5, Some(176_400.5), 44_100, [255, 100, 0])).unwrap();
        let cues = db.load_cues(path);
        assert_eq!(cues.len(), 2);
        assert_eq!(cues[0].hot_cue, Some(1));
        assert!(!cues[0].is_loop());
        assert_eq!(cues[1].pos_frames, 88_200.5, "exact frame position survives");
        assert_eq!(cues[1].out_frames, Some(176_400.5));

        // Re-setting a pad replaces it instead of adding a second row.
        db.save_cue(path, &Cue::new(Some(1), 22_050.0, None, 44_100, [0, 200, 80])).unwrap();
        let cues = db.load_cues(path);
        assert_eq!(cues.len(), 2);
        assert_eq!(cues[0].pos_frames, 22_050.0);

        db.delete_cue(path, 2).unwrap();
        assert_eq!(db.load_cues(path).len(), 1);
    }

    fn tags(title: &str, genre: &str) -> TrackTags {
        TrackTags {
            title: Some(title.into()),
            artist: Some("Artist".into()),
            album: None,
            genre: Some(genre.into()),
            year: Some(2020),
        }
    }

    #[test]
    fn index_then_analysis_fills_the_browser_row() {
        let db = mem_db_migrated();
        let p = Path::new("/music/a.mp3");
        db.index_file(p, &tags("Song", "House"), 100, 5000).unwrap();
        assert_eq!(db.pending_analysis(), vec![p.to_path_buf()]);

        db.save_analysis(p, 126.0, 215.5, Some((9, true, 0.4)), &[[1, 2, 3], [4, 5, 6]]).unwrap();
        assert!(db.pending_analysis().is_empty());
        let rows = db.all_tracks();
        assert_eq!(rows.len(), 1);
        let r = &rows[0];
        assert_eq!(r.display_title(), "Song");
        assert_eq!(r.genre.as_deref(), Some("House"));
        assert_eq!(r.bpm, Some(126.0));
        assert_eq!(r.key, Some((9, true)));
        assert_eq!(r.preview, vec![[1, 2, 3], [4, 5, 6]]);
        assert!(r.analysed);

        // Same file again: tags refresh but analysis is kept.
        db.index_file(p, &tags("Song 2", "House"), 100, 5000).unwrap();
        assert!(db.pending_analysis().is_empty());
        // A changed file is queued again.
        db.index_file(p, &tags("Song 2", "House"), 101, 5001).unwrap();
        assert_eq!(db.pending_analysis().len(), 1);
    }

    #[test]
    fn untagged_files_show_their_file_name() {
        let db = mem_db_migrated();
        db.index_file(Path::new("/music/My Tune.flac"), &TrackTags::default(), 1, 1).unwrap();
        assert_eq!(db.all_tracks()[0].display_title(), "My Tune");
    }

    #[test]
    fn manual_bpm_beats_the_analysed_tempo_in_the_browser() {
        let db = mem_db_migrated();
        let p = Path::new("/music/a.mp3");
        db.index_file(p, &tags("S", "House"), 1, 1).unwrap();
        db.save_analysis(p, 63.0, 200.0, None, &[]).unwrap();
        db.save_grid(p, BeatGrid { bpm: 126.0, offset: 0.0 }).unwrap();
        let r = &db.all_tracks()[0];
        assert_eq!(r.bpm, Some(126.0));
        assert!(r.bpm_manual);
    }

    #[test]
    fn a_newer_analysis_marks_older_tracks_for_analysis_again() {
        let db = mem_db_migrated();
        let p = Path::new("/m/pulsar.ogg");
        db.upsert_track(p, 200.0, 91.3).unwrap();
        db.save_analysis(p, 91.3, 200.0, None, &[]).unwrap();
        let analysed = |db: &LibraryDb| db.all_tracks().iter().find(|r| r.path == p).is_some_and(|r| r.analysed);
        assert!(analysed(&db));
        {
            let c = db.conn.lock().unwrap();
            refresh_stale_analysis(&c, 7); // first sight of a version: redo what was analysed before
        }
        assert!(!analysed(&db));
        db.save_analysis(p, 137.0, 200.0, None, &[]).unwrap();
        {
            let c = db.conn.lock().unwrap();
            refresh_stale_analysis(&c, 7); // same version again: leave it
        }
        assert!(analysed(&db));
        assert_eq!(db.track_bpm(p), Some(137.0));
        {
            let c = db.conn.lock().unwrap();
            refresh_stale_analysis(&c, 8); // the next version: redo again
        }
        assert!(!analysed(&db));
    }

    #[test]
    fn reels_hold_tracks_in_order_and_a_track_can_be_on_many_reels() {
        let db = mem_db_migrated();
        let (a, b, c) = (Path::new("/m/a.mp3"), Path::new("/m/b.mp3"), Path::new("/m/c.mp3"));
        let warm = db.create_reel("Warm-up").unwrap();
        let peak = db.create_reel("Peak time").unwrap();
        assert_eq!(db.create_reel("warm-up").unwrap(), warm, "names are unique, case-insensitive");

        db.add_to_reel(warm, a).unwrap();
        db.add_to_reel(warm, b).unwrap();
        db.add_to_reel(warm, a).unwrap(); // duplicate ignored
        db.add_to_reel(peak, b).unwrap();
        db.add_to_reel(peak, c).unwrap();

        assert_eq!(db.reel_paths(warm), vec![a.to_path_buf(), b.to_path_buf()]);
        assert_eq!(db.reel_paths(peak), vec![b.to_path_buf(), c.to_path_buf()]);
        let mut of_b = db.reels_of(b);
        of_b.sort();
        assert_eq!(of_b, vec![warm, peak]);
        let counts: Vec<_> = db.reels().into_iter().map(|(_, n, c)| (n, c)).collect();
        assert_eq!(counts, vec![("Peak time".to_string(), 2), ("Warm-up".to_string(), 2)]);

        db.remove_from_reel(warm, a).unwrap();
        assert_eq!(db.reel_paths(warm), vec![b.to_path_buf()]);
        db.delete_reel(peak).unwrap();
        assert!(db.reel_paths(peak).is_empty());
        assert_eq!(db.reels().len(), 1);
        assert_eq!(db.reel_paths(warm), vec![b.to_path_buf()], "other reels are untouched");
    }

    #[test]
    fn sources_add_remove_and_dedupe() {
        let db = mem_db_migrated();
        db.add_source(Path::new("/music")).unwrap();
        db.add_source(Path::new("/music")).unwrap();
        db.add_source(Path::new("/other")).unwrap();
        assert_eq!(db.sources().len(), 2);
        db.remove_source(Path::new("/music")).unwrap();
        assert_eq!(db.sources(), vec![PathBuf::from("/other")]);
    }

    #[test]
    fn main_cue_round_trips() {
        let db = mem_db_migrated();
        let path = Path::new("/music/track.ogg");
        assert_eq!(db.load_main_cue(path), None);
        db.save_main_cue(path, 123_456.25).unwrap();
        assert_eq!(db.load_main_cue(path), Some(123_456.25));
        db.save_main_cue(path, 5.0).unwrap();
        assert_eq!(db.load_main_cue(path), Some(5.0));
    }

    #[test]
    fn manual_grid_override_round_trips_and_clears() {
        let db = mem_db();
        let path = Path::new("/music/track.ogg");
        assert!(db.load_grid(path).is_none());

        db.save_grid(path, BeatGrid { bpm: 127.5, offset: 321.25 }).unwrap();
        let g = db.load_grid(path).unwrap();
        assert!((g.bpm - 127.5).abs() < 1e-4 && (g.offset - 321.25).abs() < 1e-9);

        db.save_grid(path, BeatGrid { bpm: 128.0, offset: 10.0 }).unwrap();
        assert!((db.load_grid(path).unwrap().bpm - 128.0).abs() < 1e-4);

        db.clear_grid(path).unwrap();
        assert!(db.load_grid(path).is_none());
    }
}
