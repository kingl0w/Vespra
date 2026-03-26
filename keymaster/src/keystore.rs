use alloy::primitives::U256;
use chrono::Utc;
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::Mutex;

use crate::error::{AppError, AppResult};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeeSweep {
    pub id: i64,
    pub sweep_type: String,
    pub aum_eth: Option<f64>,
    pub accrual_eth: f64,
    pub tx_hash: Option<String>,
    pub swept: i64,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WalletRecord {
    pub id: String,
    pub address: String,
    pub chain: String,
    pub label: String,
    #[serde(skip_serializing)]
    pub encrypted_key: String,
    pub cap_wei: String,
    pub strategy: String,
    pub active: bool,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WalletInfo {
    pub id: String,
    pub address: String,
    pub chain: String,
    pub label: String,
    pub cap_wei: String,
    pub strategy: String,
    pub active: bool,
    pub created_at: String,
    pub updated_at: String,
}

impl From<WalletRecord> for WalletInfo {
    fn from(r: WalletRecord) -> Self {
        Self {
            id: r.id,
            address: r.address,
            chain: r.chain,
            label: r.label,
            cap_wei: r.cap_wei,
            strategy: r.strategy,
            active: r.active,
            created_at: r.created_at,
            updated_at: r.updated_at,
        }
    }
}

pub struct Keystore {
    conn: Mutex<Connection>,
}

impl Keystore {
    pub fn open(path: &Path) -> AppResult<Self> {
        let conn = Connection::open(path)?;
        conn.execute_batch("PRAGMA journal_mode=WAL;")?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS wallets (
                id TEXT PRIMARY KEY,
                address TEXT NOT NULL UNIQUE,
                chain TEXT NOT NULL,
                label TEXT NOT NULL DEFAULT '',
                encrypted_key TEXT NOT NULL,
                cap_wei TEXT NOT NULL DEFAULT '0',
                strategy TEXT NOT NULL DEFAULT '',
                active INTEGER NOT NULL DEFAULT 1,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_wallets_chain ON wallets(chain);
            CREATE INDEX IF NOT EXISTS idx_wallets_strategy ON wallets(strategy);
            CREATE INDEX IF NOT EXISTS idx_wallets_active ON wallets(active);
            CREATE TABLE IF NOT EXISTS tx_log (
                id TEXT PRIMARY KEY,
                wallet_id TEXT NOT NULL,
                chain TEXT NOT NULL,
                tx_hash TEXT,
                tx_type TEXT NOT NULL,
                to_address TEXT NOT NULL,
                value_wei TEXT NOT NULL DEFAULT '0',
                status TEXT NOT NULL DEFAULT 'pending',
                error TEXT,
                created_at TEXT NOT NULL,
                FOREIGN KEY (wallet_id) REFERENCES wallets(id)
            );
            CREATE INDEX IF NOT EXISTS idx_tx_wallet ON tx_log(wallet_id);
            CREATE INDEX IF NOT EXISTS idx_tx_chain ON tx_log(chain);
            CREATE TABLE IF NOT EXISTS settings (
                key TEXT PRIMARY KEY,
                value TEXT NOT NULL,
                updated_at TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS fee_sweeps (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                sweep_type TEXT NOT NULL,
                aum_eth REAL,
                accrual_eth REAL NOT NULL,
                tx_hash TEXT,
                swept INTEGER NOT NULL DEFAULT 0,
                created_at TEXT NOT NULL DEFAULT (datetime('now'))
            );",
        )?;
        Ok(Self { conn: Mutex::new(conn) })
    }

    pub fn insert_wallet(&self, wallet: &WalletRecord) -> AppResult<()> {
        let conn = self.conn.lock().map_err(|e| AppError::Internal(e.to_string()))?;
        conn.execute(
            "INSERT INTO wallets (id, address, chain, label, encrypted_key, cap_wei, strategy, active, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                wallet.id, wallet.address, wallet.chain, wallet.label,
                wallet.encrypted_key, wallet.cap_wei, wallet.strategy,
                wallet.active, wallet.created_at, wallet.updated_at,
            ],
        )?;
        Ok(())
    }

    pub fn get_wallet(&self, id: &str) -> AppResult<WalletRecord> {
        let conn = self.conn.lock().map_err(|e| AppError::Internal(e.to_string()))?;
        conn.query_row(
            "SELECT id, address, chain, label, encrypted_key, cap_wei, strategy, active, created_at, updated_at
             FROM wallets WHERE id = ?1",
            params![id],
            |row| {
                Ok(WalletRecord {
                    id: row.get(0)?, address: row.get(1)?, chain: row.get(2)?,
                    label: row.get(3)?, encrypted_key: row.get(4)?, cap_wei: row.get(5)?,
                    strategy: row.get(6)?, active: row.get(7)?,
                    created_at: row.get(8)?, updated_at: row.get(9)?,
                })
            },
        ).map_err(|_| AppError::WalletNotFound(id.to_string()))
    }

    pub fn list_wallets(&self, chain: Option<&str>, strategy: Option<&str>) -> AppResult<Vec<WalletInfo>> {
        let conn = self.conn.lock().map_err(|e| AppError::Internal(e.to_string()))?;
        let mut sql = String::from(
            "SELECT id, address, chain, label, encrypted_key, cap_wei, strategy, active, created_at, updated_at
             FROM wallets WHERE 1=1"
        );
        let mut param_values: Vec<String> = Vec::new();

        if let Some(c) = chain {
            param_values.push(c.to_string());
            sql.push_str(&format!(" AND chain = ?{}", param_values.len()));
        }
        if let Some(s) = strategy {
            param_values.push(s.to_string());
            sql.push_str(&format!(" AND strategy = ?{}", param_values.len()));
        }
        sql.push_str(" ORDER BY created_at DESC");

        let mut stmt = conn.prepare(&sql)?;
        let params: Vec<&dyn rusqlite::ToSql> = param_values.iter().map(|v| v as &dyn rusqlite::ToSql).collect();
        let wallets = stmt
            .query_map(params.as_slice(), |row| {
                Ok(WalletRecord {
                    id: row.get(0)?, address: row.get(1)?, chain: row.get(2)?,
                    label: row.get(3)?, encrypted_key: row.get(4)?, cap_wei: row.get(5)?,
                    strategy: row.get(6)?, active: row.get(7)?,
                    created_at: row.get(8)?, updated_at: row.get(9)?,
                })
            })?
            .filter_map(|r| r.ok())
            .map(WalletInfo::from)
            .collect();
        Ok(wallets)
    }

    pub fn deactivate_wallet(&self, id: &str) -> AppResult<()> {
        let conn = self.conn.lock().map_err(|e| AppError::Internal(e.to_string()))?;
        let now = Utc::now().to_rfc3339();
        let rows = conn.execute(
            "UPDATE wallets SET active = 0, updated_at = ?1 WHERE id = ?2",
            params![now, id],
        )?;
        if rows == 0 { return Err(AppError::WalletNotFound(id.to_string())); }
        Ok(())
    }

    pub fn update_cap(&self, id: &str, cap_wei: &str) -> AppResult<()> {
        let conn = self.conn.lock().map_err(|e| AppError::Internal(e.to_string()))?;
        let now = Utc::now().to_rfc3339();
        let rows = conn.execute(
            "UPDATE wallets SET cap_wei = ?1, updated_at = ?2 WHERE id = ?3",
            params![cap_wei, now, id],
        )?;
        if rows == 0 { return Err(AppError::WalletNotFound(id.to_string())); }
        Ok(())
    }

    pub fn log_tx(
        &self, wallet_id: &str, chain: &str, tx_hash: Option<&str>,
        tx_type: &str, to_address: &str, value_wei: &str, status: &str, error: Option<&str>,
    ) -> AppResult<String> {
        let conn = self.conn.lock().map_err(|e| AppError::Internal(e.to_string()))?;
        let id = uuid::Uuid::new_v4().to_string();
        let now = Utc::now().to_rfc3339();
        conn.execute(
            "INSERT INTO tx_log (id, wallet_id, chain, tx_hash, tx_type, to_address, value_wei, status, error, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![id, wallet_id, chain, tx_hash, tx_type, to_address, value_wei, status, error, now],
        )?;
        Ok(id)
    }

    pub fn get_setting(&self, key: &str) -> AppResult<Option<String>> {
        let conn = self.conn.lock().map_err(|e| AppError::Internal(e.to_string()))?;
        match conn.query_row(
            "SELECT value FROM settings WHERE key = ?1",
            params![key],
            |row| row.get::<_, String>(0),
        ) {
            Ok(val) => Ok(Some(val)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    pub fn set_setting(&self, key: &str, value: &str) -> AppResult<()> {
        let conn = self.conn.lock().map_err(|e| AppError::Internal(e.to_string()))?;
        let now = Utc::now().to_rfc3339();
        conn.execute(
            "INSERT INTO settings (key, value, updated_at) VALUES (?1, ?2, ?3)
             ON CONFLICT(key) DO UPDATE SET value = ?2, updated_at = ?3",
            params![key, value, now],
        )?;
        Ok(())
    }

    pub fn list_settings_by_prefix(&self, prefix: &str) -> AppResult<Vec<(String, String)>> {
        let conn = self.conn.lock().map_err(|e| AppError::Internal(e.to_string()))?;
        let mut stmt = conn.prepare(
            "SELECT key, value FROM settings WHERE key LIKE ?1 ORDER BY key",
        )?;
        let pattern = format!("{prefix}%");
        let rows = stmt
            .query_map(params![pattern], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?
            .filter_map(|r| r.ok())
            .collect();
        Ok(rows)
    }

    pub fn total_sent_wei(&self, wallet_id: &str) -> AppResult<U256> {
        let conn = self.conn.lock().map_err(|e| AppError::Internal(e.to_string()))?;
        let mut stmt = conn.prepare(
            "SELECT COALESCE(value_wei, '0') FROM tx_log
             WHERE wallet_id = ?1 AND tx_type = 'send_native' AND status = 'confirmed'",
        )?;
        let rows: Vec<String> = stmt
            .query_map(params![wallet_id], |row| row.get::<_, String>(0))?
            .filter_map(|r| r.ok())
            .collect();
        let mut total = U256::ZERO;
        for val in &rows {
            if let Ok(n) = val.parse::<u128>() {
                total += U256::from(n);
            }
        }
        Ok(total)
    }

    pub fn get_tx_log(&self, wallet_id: &str, limit: usize) -> AppResult<Vec<serde_json::Value>> {
        let conn = self.conn.lock().map_err(|e| AppError::Internal(e.to_string()))?;
        let mut stmt = conn.prepare(
            "SELECT id, tx_hash, tx_type, to_address, value_wei, status, error, created_at
             FROM tx_log WHERE wallet_id = ?1 ORDER BY created_at DESC LIMIT ?2",
        )?;
        let rows = stmt
            .query_map(params![wallet_id, limit as i64], |row| {
                Ok(serde_json::json!({
                    "id": row.get::<_, String>(0)?,
                    "tx_hash": row.get::<_, Option<String>>(1)?,
                    "tx_type": row.get::<_, String>(2)?,
                    "to_address": row.get::<_, String>(3)?,
                    "value_wei": row.get::<_, String>(4)?,
                    "status": row.get::<_, String>(5)?,
                    "error": row.get::<_, Option<String>>(6)?,
                    "created_at": row.get::<_, String>(7)?,
                }))
            })?
            .filter_map(|r| r.ok())
            .collect();
        Ok(rows)
    }

    // ─── Fee Sweep Helpers ───────────────────────────────────────

    pub fn insert_fee_sweep(
        &self, sweep_type: &str, aum_eth: Option<f64>, accrual_eth: f64,
        tx_hash: Option<&str>, swept: bool,
    ) -> AppResult<()> {
        let conn = self.conn.lock().map_err(|e| AppError::Internal(e.to_string()))?;
        conn.execute(
            "INSERT INTO fee_sweeps (sweep_type, aum_eth, accrual_eth, tx_hash, swept)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![sweep_type, aum_eth, accrual_eth, tx_hash, swept as i64],
        )?;
        Ok(())
    }

    pub fn get_last_aum_sweep_time(&self) -> AppResult<Option<i64>> {
        let conn = self.conn.lock().map_err(|e| AppError::Internal(e.to_string()))?;
        match conn.query_row(
            "SELECT strftime('%s', created_at) FROM fee_sweeps
             WHERE sweep_type = 'aum' ORDER BY id DESC LIMIT 1",
            [],
            |row| row.get::<_, String>(0),
        ) {
            Ok(ts_str) => Ok(ts_str.parse::<i64>().ok()),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    pub fn get_fee_sweeps(&self, limit: i64) -> AppResult<Vec<FeeSweep>> {
        let conn = self.conn.lock().map_err(|e| AppError::Internal(e.to_string()))?;
        let mut stmt = conn.prepare(
            "SELECT id, sweep_type, aum_eth, accrual_eth, tx_hash, swept, created_at
             FROM fee_sweeps ORDER BY id DESC LIMIT ?1",
        )?;
        let rows = stmt
            .query_map(params![limit], |row| {
                Ok(FeeSweep {
                    id: row.get(0)?,
                    sweep_type: row.get(1)?,
                    aum_eth: row.get(2)?,
                    accrual_eth: row.get(3)?,
                    tx_hash: row.get(4)?,
                    swept: row.get(5)?,
                    created_at: row.get(6)?,
                })
            })?
            .filter_map(|r| r.ok())
            .collect();
        Ok(rows)
    }
}
