//! Persistent usage history: per-minute average and peak per series, kept in
//! SQLite, downsampled to 10-minute rows after a while and pruned at retention.

use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};
use std::path::Path;

use crate::config::HistoryConfig;

const SCHEMA: &str = "
PRAGMA auto_vacuum = INCREMENTAL;
PRAGMA journal_mode = WAL;
PRAGMA synchronous = NORMAL;
CREATE TABLE IF NOT EXISTS series(
    id INTEGER PRIMARY KEY,
    kind TEXT NOT NULL,
    key TEXT NOT NULL,
    label TEXT NOT NULL,
    tokens INTEGER,
    UNIQUE(kind, key)
);
CREATE TABLE IF NOT EXISTS samples(
    series_id INTEGER NOT NULL,
    ts INTEGER NOT NULL,
    bucket INTEGER NOT NULL,
    cpu_avg REAL NOT NULL,
    cpu_max REAL NOT NULL,
    mem_avg REAL NOT NULL,
    mem_max REAL NOT NULL,
    PRIMARY KEY(series_id, ts)
) WITHOUT ROWID;
CREATE INDEX IF NOT EXISTS samples_ts ON samples(ts);
";

const DOWNSAMPLE_BUCKET: u64 = 600;

#[derive(Debug, Default, Clone)]
struct Acc {
    label: String,
    n: u64,
    cpu_sum: f64,
    cpu_max: f64,
    mem_sum: f64,
    mem_max: f64,
    /// Latest token total of an agent session.
    tokens: Option<u64>,
}

type SeriesKey = (&'static str, String);

pub struct History {
    conn: Connection,
    acc: HashMap<SeriesKey, Acc>,
    ids: HashMap<SeriesKey, (i64, String)>,
    bucket: u64,
    rollup: u64,
    retention: u64,
    downsample_after: u64,
    top_apps: usize,
    last_maintenance: u64,
}

pub fn open_connection(path: &Path) -> rusqlite::Result<Connection> {
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let conn = Connection::open(path)?;
    conn.execute_batch(SCHEMA)?;
    migrate(&conn)?;
    Ok(conn)
}

/// Adds columns that databases created by older versions lack.
fn migrate(conn: &Connection) -> rusqlite::Result<()> {
    let has_tokens = conn
        .prepare("SELECT 1 FROM pragma_table_info('series') WHERE name = 'tokens'")?
        .exists([])?;
    if !has_tokens {
        conn.execute_batch("ALTER TABLE series ADD COLUMN tokens INTEGER")?;
    }
    Ok(())
}

impl History {
    pub fn open(path: &Path, cfg: &HistoryConfig) -> rusqlite::Result<Self> {
        Ok(Self::with_connection(open_connection(path)?, cfg))
    }

    pub fn with_connection(conn: Connection, cfg: &HistoryConfig) -> Self {
        Self {
            conn,
            acc: HashMap::new(),
            ids: HashMap::new(),
            bucket: 0,
            rollup: cfg.rollup_secs,
            retention: cfg.retention_days * 86_400,
            downsample_after: cfg.downsample_after_days * 86_400,
            top_apps: cfg.top_apps,
            last_maintenance: 0,
        }
    }

    pub fn connection(&self) -> &Connection {
        &self.conn
    }

    /// Call once per sample, before `record`: writes the previous bucket when a
    /// new one starts and runs maintenance once an hour.
    pub fn tick(&mut self, now: u64) -> rusqlite::Result<()> {
        let bucket = now / self.rollup * self.rollup;
        if self.bucket == 0 {
            self.bucket = bucket;
        }
        if bucket != self.bucket {
            self.flush()?;
            self.bucket = bucket;
        }
        if now >= self.last_maintenance + 3600 {
            self.maintain(now)?;
            self.last_maintenance = now;
        }
        Ok(())
    }

    pub fn record(&mut self, kind: &'static str, key: &str, label: &str, cpu: f64, mem: f64) {
        let a = self.acc.entry((kind, key.to_string())).or_default();
        if a.label != label {
            a.label = label.to_string();
        }
        a.n += 1;
        a.cpu_sum += cpu;
        a.cpu_max = a.cpu_max.max(cpu);
        a.mem_sum += mem;
        a.mem_max = a.mem_max.max(mem);
    }

    /// Remembers an agent session's token total; written with its next row.
    pub fn record_tokens(&mut self, kind: &'static str, key: &str, tokens: u64) {
        if let Some(a) = self.acc.get_mut(&(kind, key.to_string())) {
            a.tokens = Some(tokens);
        }
    }

    pub fn flush(&mut self) -> rusqlite::Result<()> {
        let acc = std::mem::take(&mut self.acc);
        if acc.is_empty() {
            return Ok(());
        }
        let mut apps: Vec<(&SeriesKey, f64)> = acc
            .iter()
            .filter(|((kind, _), a)| *kind == "app" && a.n > 0)
            .map(|(k, a)| (k, a.mem_sum / a.n as f64))
            .collect();
        apps.sort_by(|a, b| b.1.total_cmp(&a.1));
        let keep: HashSet<SeriesKey> = apps
            .iter()
            .take(self.top_apps)
            .map(|(k, _)| (*k).clone())
            .collect();
        let tx = self.conn.transaction()?;
        for (key, a) in &acc {
            if a.n == 0 || (key.0 == "app" && !keep.contains(key)) {
                continue;
            }
            let id = series_id(&tx, &mut self.ids, key, &a.label)?;
            let n = a.n as f64;
            tx.execute(
                "INSERT OR REPLACE INTO samples VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    id,
                    self.bucket as i64,
                    self.rollup as i64,
                    a.cpu_sum / n,
                    a.cpu_max,
                    a.mem_sum / n,
                    a.mem_max
                ],
            )?;
            if let Some(t) = a.tokens {
                tx.execute(
                    "UPDATE series SET tokens = ?1 WHERE id = ?2",
                    params![t as i64, id],
                )?;
            }
        }
        tx.commit()
    }

    pub fn maintain(&mut self, now: u64) -> rusqlite::Result<()> {
        let downsample_before =
            now.saturating_sub(self.downsample_after) / DOWNSAMPLE_BUCKET * DOWNSAMPLE_BUCKET;
        let delete_before = now.saturating_sub(self.retention);
        let tx = self.conn.transaction()?;
        tx.execute(
            "INSERT OR REPLACE INTO samples
             SELECT series_id, (ts / ?1) * ?1, ?1,
                    SUM(cpu_avg * bucket) / SUM(bucket), MAX(cpu_max),
                    SUM(mem_avg * bucket) / SUM(bucket), MAX(mem_max)
             FROM samples WHERE bucket < ?1 AND ts < ?2
             GROUP BY series_id, (ts / ?1)",
            params![DOWNSAMPLE_BUCKET as i64, downsample_before as i64],
        )?;
        tx.execute(
            "DELETE FROM samples WHERE bucket < ?1 AND ts < ?2",
            params![DOWNSAMPLE_BUCKET as i64, downsample_before as i64],
        )?;
        tx.execute(
            "DELETE FROM samples WHERE ts < ?1",
            params![delete_before as i64],
        )?;
        tx.execute(
            "DELETE FROM series WHERE id NOT IN (SELECT DISTINCT series_id FROM samples)",
            [],
        )?;
        tx.commit()?;
        self.ids.clear();
        self.conn.execute_batch("PRAGMA incremental_vacuum;")
    }
}

fn series_id(
    conn: &Connection,
    ids: &mut HashMap<SeriesKey, (i64, String)>,
    key: &SeriesKey,
    label: &str,
) -> rusqlite::Result<i64> {
    if let Some((id, known)) = ids.get(key)
        && known == label
    {
        return Ok(*id);
    }
    let id: i64 = conn.query_row(
        "INSERT INTO series(kind, key, label) VALUES (?1, ?2, ?3)
         ON CONFLICT(kind, key) DO UPDATE SET label = excluded.label
         RETURNING id",
        params![key.0, key.1, label],
        |r| r.get(0),
    )?;
    ids.insert(key.clone(), (id, label.to_string()));
    Ok(id)
}

pub fn range_secs(range: &str) -> u64 {
    match range {
        "1h" => 3600,
        "7d" => 7 * 86_400,
        "30d" => 30 * 86_400,
        _ => 86_400,
    }
}

/// History for one series (`kind:key`, default the system) plus a ranking of
/// sessions, apps and containers over the range. At most ~120 points.
pub fn query(conn: &Connection, range: &str, target: &str, now: u64) -> rusqlite::Result<Value> {
    let span = range_secs(range);
    let from = now.saturating_sub(span);
    let step = (span / 120).max(60).div_ceil(60) * 60;
    let (kind, key) = target.split_once(':').unwrap_or(("system", "system"));
    let series: Option<(i64, String)> = conn
        .query_row(
            "SELECT id, label FROM series WHERE kind = ?1 AND key = ?2",
            params![kind, key],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let mut points: Vec<[f64; 5]> = Vec::new();
    if let Some((id, _)) = &series {
        let mut st = conn.prepare(
            "SELECT (ts / ?1) * ?1 AS b,
                    SUM(cpu_avg * bucket) / SUM(bucket), MAX(cpu_max),
                    SUM(mem_avg * bucket) / SUM(bucket), MAX(mem_max)
             FROM samples WHERE series_id = ?2 AND ts >= ?3
             GROUP BY b ORDER BY b",
        )?;
        let rows = st.query_map(params![step as i64, id, from as i64], |r| {
            Ok([
                r.get::<_, i64>(0)? as f64,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get(4)?,
            ])
        })?;
        for row in rows {
            points.push(row?);
        }
    }
    let mut st = conn.prepare(
        "SELECT s.kind, s.key, s.label,
                SUM(x.cpu_avg * x.bucket) / SUM(x.bucket), MAX(x.cpu_max),
                SUM(x.mem_avg * x.bucket) / SUM(x.bucket), MAX(x.mem_max),
                SUM(x.bucket) / 60, s.tokens
         FROM samples x JOIN series s ON s.id = x.series_id
         WHERE x.ts >= ?1 AND s.kind != 'system'
         GROUP BY s.id ORDER BY 6 DESC LIMIT 15",
    )?;
    let ranking: Vec<Value> = st
        .query_map(params![from as i64], |r| {
            Ok(json!({
                "kind": r.get::<_, String>(0)?,
                "key": r.get::<_, String>(1)?,
                "label": r.get::<_, String>(2)?,
                "cpu_avg": r.get::<_, f64>(3)?,
                "cpu_max": r.get::<_, f64>(4)?,
                "mem_avg": r.get::<_, f64>(5)?,
                "mem_max": r.get::<_, f64>(6)?,
                "minutes": r.get::<_, i64>(7)?,
                "tokens": r.get::<_, Option<i64>>(8)?,
            }))
        })?
        .collect::<rusqlite::Result<_>>()?;
    Ok(json!({
        "range": range,
        "from": from,
        "to": now,
        "step": step,
        "target": { "kind": kind, "key": key, "label": series.map(|s| s.1) },
        "points": points,
        "ranking": ranking,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mem_history() -> History {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(SCHEMA).unwrap();
        History::with_connection(
            conn,
            &HistoryConfig {
                top_apps: 1,
                ..HistoryConfig::default()
            },
        )
    }

    fn count(h: &History, sql: &str) -> i64 {
        h.connection().query_row(sql, [], |r| r.get(0)).unwrap()
    }

    #[test]
    fn rollup_keeps_avg_and_peak_and_top_apps_only() {
        let mut h = mem_history();
        let t0 = 1_800_000_000 / 60 * 60;
        h.tick(t0).unwrap();
        for (cpu, mem) in [(10.0, 100.0), (30.0, 300.0)] {
            h.record("system", "system", "System", cpu, mem);
            h.record("app", "big", "big", 1.0, 900.0);
            h.record("app", "small", "small", 1.0, 10.0);
        }
        h.tick(t0 + 60).unwrap();
        let (avg, max): (f64, f64) = h
            .connection()
            .query_row("SELECT cpu_avg, mem_max FROM samples JOIN series ON id = series_id WHERE kind = 'system'", [], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap();
        assert_eq!(avg, 20.0);
        assert_eq!(max, 300.0);
        assert_eq!(
            count(&h, "SELECT COUNT(*) FROM series WHERE kind = 'app'"),
            1
        );
    }

    #[test]
    fn maintenance_downsamples_and_prunes() {
        let mut h = mem_history();
        let now: u64 = 1_800_000_000 / 600 * 600;
        let old = now - 10 * 86_400;
        let ancient = now - 40 * 86_400;
        for (i, ts) in [old, old + 60, old + 120, ancient].into_iter().enumerate() {
            h.bucket = ts;
            h.record("system", "system", "System", i as f64 * 10.0, 100.0);
            h.flush().unwrap();
        }
        h.maintain(now).unwrap();
        assert_eq!(count(&h, "SELECT COUNT(*) FROM samples"), 1);
        let (bucket, avg, max): (i64, f64, f64) = h
            .connection()
            .query_row("SELECT bucket, cpu_avg, cpu_max FROM samples", [], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?))
            })
            .unwrap();
        assert_eq!(bucket, 600);
        assert_eq!(avg, 10.0);
        assert_eq!(max, 20.0);
    }

    #[test]
    fn query_returns_points_and_ranking() {
        let mut h = mem_history();
        let now: u64 = 1_800_000_000;
        h.bucket = now - 120;
        h.record("system", "system", "System", 50.0, 1000.0);
        h.record("session", "sid", "acme-api-99", 120.0, 2e9);
        h.flush().unwrap();
        let v = query(h.connection(), "1h", "", now).unwrap();
        assert_eq!(v["points"].as_array().unwrap().len(), 1);
        assert_eq!(v["ranking"][0]["label"], "acme-api-99");
        let v = query(h.connection(), "1h", "session:sid", now).unwrap();
        assert_eq!(v["target"]["label"], "acme-api-99");
    }

    #[test]
    fn agent_tokens_and_title_reach_the_ranking() {
        let mut h = mem_history();
        let now: u64 = 1_800_000_000;
        h.bucket = now - 120;
        h.record("session", "claude:sid", "hivetop-97", 10.0, 1e9);
        h.record("session", "claude:sid", "Popup layout", 10.0, 1e9);
        h.record_tokens("session", "claude:sid", 317_000);
        h.flush().unwrap();
        let v = query(h.connection(), "1h", "session:claude:sid", now).unwrap();
        assert_eq!(v["ranking"][0]["label"], "Popup layout");
        assert_eq!(v["ranking"][0]["tokens"], 317_000);
        assert_eq!(v["target"]["key"], "claude:sid");
    }

    #[test]
    fn old_databases_gain_the_tokens_column() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE series(id INTEGER PRIMARY KEY, kind TEXT NOT NULL, key TEXT NOT NULL, label TEXT NOT NULL, UNIQUE(kind, key));",
        )
        .unwrap();
        conn.execute_batch(SCHEMA).unwrap();
        migrate(&conn).unwrap();
        migrate(&conn).unwrap();
        let n: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('series') WHERE name = 'tokens'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 1);
    }
}
