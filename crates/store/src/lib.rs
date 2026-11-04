//! Local SQLite store: a mirror of the Jellyfin items we have seen, plus the
//! original text and every translation we have produced (so the server can
//! hold only the currently selected language).

use std::path::Path;

use anyhow::{Context, Result};
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PersonRecord {
    pub id: Option<String>,
    pub name: String,
    pub role: Option<String>,
    pub person_type: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ItemRecord {
    pub id: String,
    pub library_id: String,
    pub item_type: String,
    pub name: Option<String>,
    pub original_title: Option<String>,
    pub overview: Option<String>,
    pub tags: Vec<String>,
    pub people: Vec<PersonRecord>,
    pub image_tag: Option<String>,
    pub locked_fields: Vec<String>,
    pub source_hash: String,
    pub synced_at: i64,
}

pub struct Store {
    conn: Connection,
}

impl Store {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create {}", parent.display()))?;
        }
        let conn =
            Connection::open(path).with_context(|| format!("open sqlite {}", path.display()))?;
        let store = Self { conn };
        store.migrate()?;
        Ok(store)
    }

    pub fn open_in_memory() -> Result<Self> {
        let store = Self {
            conn: Connection::open_in_memory()?,
        };
        store.migrate()?;
        Ok(store)
    }

    pub fn migrate(&self) -> Result<()> {
        self.conn.execute_batch(
            r#"
            PRAGMA journal_mode = WAL;
            PRAGMA foreign_keys = ON;

            CREATE TABLE IF NOT EXISTS items (
                id                 TEXT PRIMARY KEY,
                library_id         TEXT NOT NULL,
                item_type          TEXT NOT NULL,
                name               TEXT,
                original_title     TEXT,
                overview           TEXT,
                tags_json          TEXT NOT NULL DEFAULT '[]',
                people_json        TEXT NOT NULL DEFAULT '[]',
                image_tag          TEXT,
                locked_fields_json TEXT NOT NULL DEFAULT '[]',
                source_hash        TEXT NOT NULL,
                synced_at          INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_items_library ON items(library_id);

            CREATE TABLE IF NOT EXISTS translations (
                item_id     TEXT NOT NULL,
                field       TEXT NOT NULL,
                lang        TEXT NOT NULL,
                source_hash TEXT NOT NULL,
                text        TEXT NOT NULL,
                provider    TEXT NOT NULL,
                model       TEXT,
                status      TEXT NOT NULL DEFAULT 'draft',
                updated_at  INTEGER NOT NULL,
                PRIMARY KEY (item_id, field, lang)
            );

            CREATE TABLE IF NOT EXISTS settings (
                key   TEXT PRIMARY KEY,
                value TEXT NOT NULL
            );
            "#,
        )?;
        Ok(())
    }

    pub fn upsert_items(&mut self, items: &[ItemRecord]) -> Result<usize> {
        let tx = self.conn.transaction()?;
        {
            let mut stmt = tx.prepare(
                r#"
                INSERT INTO items (id, library_id, item_type, name, original_title, overview,
                                   tags_json, people_json, image_tag, locked_fields_json,
                                   source_hash, synced_at)
                VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
                ON CONFLICT(id) DO UPDATE SET
                    library_id = excluded.library_id,
                    item_type = excluded.item_type,
                    name = excluded.name,
                    original_title = excluded.original_title,
                    overview = excluded.overview,
                    tags_json = excluded.tags_json,
                    people_json = excluded.people_json,
                    image_tag = excluded.image_tag,
                    locked_fields_json = excluded.locked_fields_json,
                    source_hash = excluded.source_hash,
                    synced_at = excluded.synced_at
                "#,
            )?;
            for item in items {
                stmt.execute(params![
                    item.id,
                    item.library_id,
                    item.item_type,
                    item.name,
                    item.original_title,
                    item.overview,
                    serde_json::to_string(&item.tags)?,
                    serde_json::to_string(&item.people)?,
                    item.image_tag,
                    serde_json::to_string(&item.locked_fields)?,
                    item.source_hash,
                    item.synced_at,
                ])?;
            }
        }
        tx.commit()?;
        Ok(items.len())
    }

    pub fn list_items(&self, library_id: Option<&str>) -> Result<Vec<ItemRecord>> {
        let mut out = Vec::new();
        match library_id {
            Some(lib) => {
                let mut stmt = self.conn.prepare(
                    "SELECT id, library_id, item_type, name, original_title, overview, tags_json,
                            people_json, image_tag, locked_fields_json, source_hash, synced_at
                     FROM items WHERE library_id = ?1 ORDER BY name",
                )?;
                let rows = stmt.query_map([lib], Self::row_to_item)?;
                for row in rows {
                    out.push(row?);
                }
            }
            None => {
                let mut stmt = self.conn.prepare(
                    "SELECT id, library_id, item_type, name, original_title, overview, tags_json,
                            people_json, image_tag, locked_fields_json, source_hash, synced_at
                     FROM items ORDER BY name",
                )?;
                let rows = stmt.query_map([], Self::row_to_item)?;
                for row in rows {
                    out.push(row?);
                }
            }
        }
        Ok(out)
    }

    fn row_to_item(row: &rusqlite::Row<'_>) -> rusqlite::Result<ItemRecord> {
        let tags_json: String = row.get(6)?;
        let people_json: String = row.get(7)?;
        let locked_json: String = row.get(9)?;
        Ok(ItemRecord {
            id: row.get(0)?,
            library_id: row.get(1)?,
            item_type: row.get(2)?,
            name: row.get(3)?,
            original_title: row.get(4)?,
            overview: row.get(5)?,
            tags: serde_json::from_str(&tags_json).unwrap_or_default(),
            people: serde_json::from_str(&people_json).unwrap_or_default(),
            image_tag: row.get(8)?,
            locked_fields: serde_json::from_str(&locked_json).unwrap_or_default(),
            source_hash: row.get(10)?,
            synced_at: row.get(11)?,
        })
    }

    pub fn count_items(&self) -> Result<i64> {
        Ok(self
            .conn
            .query_row("SELECT COUNT(*) FROM items", [], |r| r.get(0))?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> ItemRecord {
        ItemRecord {
            id: "abc".into(),
            library_id: "lib".into(),
            item_type: "Movie".into(),
            name: Some("Test".into()),
            original_title: Some("Test".into()),
            overview: Some("overview".into()),
            tags: vec!["tag".into()],
            people: vec![PersonRecord {
                id: Some("p1".into()),
                name: "Actor".into(),
                role: Some("Role".into()),
                person_type: Some("Actor".into()),
            }],
            image_tag: Some("tag1".into()),
            locked_fields: vec!["Name".into()],
            source_hash: "hash".into(),
            synced_at: 1,
        }
    }

    #[test]
    fn upsert_and_list_roundtrip() {
        let mut store = Store::open_in_memory().unwrap();
        assert_eq!(store.upsert_items(&[sample()]).unwrap(), 1);
        assert_eq!(store.count_items().unwrap(), 1);

        let items = store.list_items(Some("lib")).unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].tags, vec!["tag".to_string()]);
        assert_eq!(items[0].people[0].role.as_deref(), Some("Role"));

        // Upsert replaces, does not duplicate.
        store.upsert_items(&[sample()]).unwrap();
        assert_eq!(store.count_items().unwrap(), 1);
    }
}
