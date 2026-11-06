//! Local SQLite store: a mirror of the Jellyfin items we have seen, plus the
//! original text and the current draft translation (so the server can hold only
//! the currently selected language and our local copy keeps the source text).
//!
//! Two layers live side by side in `items`:
//!
//! * `name` / `original_title` / `overview` / `tags_json` / `people_json` — the
//!   snapshot taken from Jellyfin. Once an item has been written back these
//!   columns keep the pre-translation source text, so a later translation still
//!   starts from the original language.
//! * `draft_*` — the translation (or the user's manual edit). The GUI shows the
//!   draft when present, otherwise the snapshot. `status` tracks where the item
//!   is in the translate → write-back flow and `locked` is the app-level lock.

use std::collections::HashMap;
use std::path::Path;

use anyhow::{Context, Result};
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};

/// Item has never been translated or edited.
pub const STATUS_UNTRANSLATED: &str = "untranslated";
/// A draft translation (or manual edit) exists locally but is not on the server.
pub const STATUS_DRAFT: &str = "draft";
/// The draft has been written back to Jellyfin.
pub const STATUS_WRITTEN: &str = "written";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LibraryRecord {
    pub id: String,
    pub name: String,
    pub collection_type: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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
    /// Original `Name` from Jellyfin (and the pre-write-back source snapshot).
    pub name: Option<String>,
    pub original_title: Option<String>,
    pub overview: Option<String>,
    pub tags: Vec<String>,
    pub people: Vec<PersonRecord>,
    pub image_tag: Option<String>,
    /// Fields locked *on the server* (`MetadataField` names), not the app lock.
    pub locked_fields: Vec<String>,
    pub source_hash: String,
    pub synced_at: i64,
    /// One of [`STATUS_UNTRANSLATED`], [`STATUS_DRAFT`], [`STATUS_WRITTEN`].
    pub status: String,
    /// App-level lock: locked items are skipped by translate/write-back.
    pub locked: bool,
    pub draft_name: Option<String>,
    pub draft_original_title: Option<String>,
    pub draft_overview: Option<String>,
    pub draft_tags: Option<Vec<String>>,
    pub draft_people: Option<Vec<PersonRecord>>,
}

impl Default for ItemRecord {
    fn default() -> Self {
        Self {
            id: String::new(),
            library_id: String::new(),
            item_type: String::new(),
            name: None,
            original_title: None,
            overview: None,
            tags: Vec::new(),
            people: Vec::new(),
            image_tag: None,
            locked_fields: Vec::new(),
            source_hash: String::new(),
            synced_at: 0,
            status: STATUS_UNTRANSLATED.to_string(),
            locked: false,
            draft_name: None,
            draft_original_title: None,
            draft_overview: None,
            draft_tags: None,
            draft_people: None,
        }
    }
}

impl ItemRecord {
    /// The title shown in the UI: draft first, source snapshot otherwise.
    pub fn effective_name(&self) -> &str {
        draft_or(&self.draft_name, &self.name)
    }

    pub fn effective_original_title(&self) -> &str {
        draft_or(&self.draft_original_title, &self.original_title)
    }

    pub fn effective_overview(&self) -> &str {
        draft_or(&self.draft_overview, &self.overview)
    }

    pub fn effective_tags(&self) -> &[String] {
        match &self.draft_tags {
            Some(tags) => tags,
            None => &self.tags,
        }
    }

    pub fn effective_people(&self) -> &[PersonRecord] {
        match &self.draft_people {
            Some(people) => people,
            None => &self.people,
        }
    }
}

fn draft_or<'a>(draft: &'a Option<String>, source: &'a Option<String>) -> &'a str {
    draft.as_deref().or(source.as_deref()).unwrap_or("")
}

/// Values written when the user saves the edit dialog or a translation finishes.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ItemDraft {
    pub name: String,
    pub original_title: String,
    pub overview: String,
    pub tags: Vec<String>,
    pub people: Vec<PersonRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PresetRecord {
    pub source: String,
    pub target: String,
    pub uses: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TranslationRecord {
    pub item_id: String,
    pub field: String,
    pub lang: String,
    pub source_hash: String,
    pub text: String,
    pub provider: String,
    pub model: Option<String>,
    pub status: String,
    pub updated_at: i64,
}

const SELECT_ITEMS: &str = "SELECT id, library_id, item_type, name, original_title, overview, \
     tags_json, people_json, image_tag, locked_fields_json, source_hash, synced_at, status, locked, \
     draft_name, draft_original_title, draft_overview, draft_tags_json, draft_people_json FROM items";

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

            CREATE TABLE IF NOT EXISTS libraries (
                id              TEXT PRIMARY KEY,
                name            TEXT NOT NULL,
                collection_type TEXT,
                synced_at       INTEGER NOT NULL DEFAULT 0
            );

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
                synced_at          INTEGER NOT NULL,
                status             TEXT NOT NULL DEFAULT 'untranslated',
                locked             INTEGER NOT NULL DEFAULT 0,
                draft_name         TEXT,
                draft_original_title TEXT,
                draft_overview     TEXT,
                draft_tags_json    TEXT,
                draft_people_json  TEXT
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

            CREATE TABLE IF NOT EXISTS presets (
                source TEXT PRIMARY KEY,
                target TEXT NOT NULL,
                uses   INTEGER NOT NULL DEFAULT 0
            );
            CREATE UNIQUE INDEX IF NOT EXISTS idx_presets_normalized
                ON presets(lower(trim(source)));

            CREATE TABLE IF NOT EXISTS settings (
                key   TEXT PRIMARY KEY,
                value TEXT NOT NULL
            );
            "#,
        )?;

        // Databases created before the draft/status columns existed only get
        // `CREATE TABLE IF NOT EXISTS` for the new tables, so add the columns
        // explicitly. `pragma_table_info` is a table-valued function (SQLite
        // 3.16+) and lets us keep this idempotent.
        for (column, decl) in [
            ("status", "TEXT NOT NULL DEFAULT 'untranslated'"),
            ("locked", "INTEGER NOT NULL DEFAULT 0"),
            ("draft_name", "TEXT"),
            ("draft_original_title", "TEXT"),
            ("draft_overview", "TEXT"),
            ("draft_tags_json", "TEXT"),
            ("draft_people_json", "TEXT"),
        ] {
            self.ensure_column("items", column, decl)?;
        }
        Ok(())
    }

    fn ensure_column(&self, table: &str, column: &str, decl: &str) -> Result<()> {
        let exists: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM pragma_table_info(?1) WHERE name = ?2",
            params![table, column],
            |row| row.get(0),
        )?;
        if exists == 0 {
            self.conn
                .execute_batch(&format!("ALTER TABLE {table} ADD COLUMN {column} {decl}"))?;
        }
        Ok(())
    }

    // ---------------------------------------------------------------- libraries

    pub fn upsert_libraries(&mut self, libraries: &[LibraryRecord]) -> Result<usize> {
        let tx = self.conn.transaction()?;
        {
            let mut stmt = tx.prepare(
                r#"
                INSERT INTO libraries (id, name, collection_type, synced_at)
                VALUES (?1, ?2, ?3, ?4)
                ON CONFLICT(id) DO UPDATE SET
                    name = excluded.name,
                    collection_type = excluded.collection_type,
                    synced_at = excluded.synced_at
                "#,
            )?;
            let now = now_unix();
            for library in libraries {
                stmt.execute(params![
                    library.id,
                    library.name,
                    library.collection_type,
                    now
                ])?;
            }
        }
        tx.commit()?;
        Ok(libraries.len())
    }

    pub fn list_libraries(&self) -> Result<Vec<LibraryRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, name, collection_type FROM libraries ORDER BY name COLLATE NOCASE",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(LibraryRecord {
                id: row.get(0)?,
                name: row.get(1)?,
                collection_type: row.get(2)?,
            })
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    pub fn library_item_counts(&self) -> Result<HashMap<String, usize>> {
        let mut stmt = self
            .conn
            .prepare("SELECT library_id, COUNT(*) FROM items GROUP BY library_id")?;
        let rows = stmt.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })?;
        let mut out = HashMap::new();
        for row in rows {
            let (library_id, count) = row?;
            out.insert(library_id, count.max(0) as usize);
        }
        Ok(out)
    }

    // -------------------------------------------------------------------- items

    /// Insert or refresh the Jellyfin snapshot.
    ///
    /// Draft fields, `status` and the app-level `locked` flag are preserved.
    /// For items already written back, the source columns are preserved as well:
    /// the server now holds the translation, so re-syncing must not clobber the
    /// original text we need for the next translation.
    pub fn upsert_items(&mut self, items: &[ItemRecord]) -> Result<usize> {
        let tx = self.conn.transaction()?;
        {
            let mut stmt = tx.prepare(
                r#"
                INSERT INTO items (id, library_id, item_type, name, original_title, overview,
                                   tags_json, people_json, image_tag, locked_fields_json,
                                   source_hash, synced_at, status, locked,
                                   draft_name, draft_original_title, draft_overview,
                                   draft_tags_json, draft_people_json)
                VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12,
                        'untranslated', 0, NULL, NULL, NULL, NULL, NULL)
                ON CONFLICT(id) DO UPDATE SET
                    library_id = excluded.library_id,
                    item_type = excluded.item_type,
                    name = CASE WHEN items.status = 'written' THEN items.name
                                ELSE excluded.name END,
                    original_title = CASE WHEN items.status = 'written' THEN items.original_title
                                          ELSE excluded.original_title END,
                    overview = CASE WHEN items.status = 'written' THEN items.overview
                                    ELSE excluded.overview END,
                    tags_json = CASE WHEN items.status = 'written' THEN items.tags_json
                                     ELSE excluded.tags_json END,
                    people_json = CASE WHEN items.status = 'written' THEN items.people_json
                                       ELSE excluded.people_json END,
                    image_tag = excluded.image_tag,
                    locked_fields_json = CASE WHEN items.status = 'written'
                                              THEN items.locked_fields_json
                                              ELSE excluded.locked_fields_json END,
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
                let sql =
                    format!("{SELECT_ITEMS} WHERE library_id = ?1 ORDER BY name COLLATE NOCASE");
                let mut stmt = self.conn.prepare(&sql)?;
                let rows = stmt.query_map([lib], Self::row_to_item)?;
                for row in rows {
                    out.push(row?);
                }
            }
            None => {
                let sql = format!("{SELECT_ITEMS} ORDER BY name COLLATE NOCASE");
                let mut stmt = self.conn.prepare(&sql)?;
                let rows = stmt.query_map([], Self::row_to_item)?;
                for row in rows {
                    out.push(row?);
                }
            }
        }
        Ok(out)
    }

    pub fn get_item(&self, id: &str) -> Result<Option<ItemRecord>> {
        let sql = format!("{SELECT_ITEMS} WHERE id = ?1");
        let mut stmt = self.conn.prepare(&sql)?;
        let mut rows = stmt.query_map([id], Self::row_to_item)?;
        match rows.next() {
            Some(row) => Ok(Some(row?)),
            None => Ok(None),
        }
    }

    fn row_to_item(row: &rusqlite::Row<'_>) -> rusqlite::Result<ItemRecord> {
        let tags_json: String = row.get(6)?;
        let people_json: String = row.get(7)?;
        let locked_json: String = row.get(9)?;
        let draft_tags_json: Option<String> = row.get(17)?;
        let draft_people_json: Option<String> = row.get(18)?;
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
            status: row.get(12)?,
            locked: row.get::<_, i64>(13)? != 0,
            draft_name: row.get(14)?,
            draft_original_title: row.get(15)?,
            draft_overview: row.get(16)?,
            draft_tags: draft_tags_json
                .as_deref()
                .map(|raw| serde_json::from_str(raw).unwrap_or_default()),
            draft_people: draft_people_json
                .as_deref()
                .map(|raw| serde_json::from_str(raw).unwrap_or_default()),
        })
    }

    /// Persist a translation/manual edit. The item leaves `written` and becomes
    /// a `draft` again.
    pub fn save_draft(&mut self, item_id: &str, draft: &ItemDraft) -> Result<()> {
        let changed = self.conn.execute(
            "UPDATE items SET draft_name = ?2, draft_original_title = ?3, draft_overview = ?4,
                              draft_tags_json = ?5, draft_people_json = ?6, status = ?7
             WHERE id = ?1",
            params![
                item_id,
                draft.name,
                draft.original_title,
                draft.overview,
                serde_json::to_string(&draft.tags)?,
                serde_json::to_string(&draft.people)?,
                STATUS_DRAFT,
            ],
        )?;
        anyhow::ensure!(changed > 0, "item {item_id} not found");
        Ok(())
    }

    pub fn set_status(&mut self, item_id: &str, status: &str) -> Result<()> {
        let changed = self.conn.execute(
            "UPDATE items SET status = ?2 WHERE id = ?1",
            params![item_id, status],
        )?;
        anyhow::ensure!(changed > 0, "item {item_id} not found");
        Ok(())
    }

    /// Mark an item as written back and record the fields locked on the server.
    pub fn mark_written(&mut self, item_id: &str, locked_fields: &[String]) -> Result<()> {
        let changed = self.conn.execute(
            "UPDATE items SET status = ?2, locked_fields_json = ?3 WHERE id = ?1",
            params![
                item_id,
                STATUS_WRITTEN,
                serde_json::to_string(locked_fields)?
            ],
        )?;
        anyhow::ensure!(changed > 0, "item {item_id} not found");
        Ok(())
    }

    /// Record fields locked on the server after a *partial* write-back while
    /// keeping the item in the draft state (only a full write-back marks it
    /// written).
    pub fn mark_draft_with_locks(&mut self, item_id: &str, locked_fields: &[String]) -> Result<()> {
        let changed = self.conn.execute(
            "UPDATE items SET status = ?2, locked_fields_json = ?3 WHERE id = ?1",
            params![item_id, STATUS_DRAFT, serde_json::to_string(locked_fields)?],
        )?;
        anyhow::ensure!(changed > 0, "item {item_id} not found");
        Ok(())
    }

    pub fn set_locked(&mut self, ids: &[String], locked: bool) -> Result<usize> {
        let tx = self.conn.transaction()?;
        let mut changed = 0;
        {
            let mut stmt = tx.prepare("UPDATE items SET locked = ?2 WHERE id = ?1")?;
            for id in ids {
                changed += stmt.execute(params![id, i64::from(locked)])?;
            }
        }
        tx.commit()?;
        Ok(changed)
    }

    pub fn count_items(&self) -> Result<i64> {
        Ok(self
            .conn
            .query_row("SELECT COUNT(*) FROM items", [], |r| r.get(0))?)
    }

    // ------------------------------------------------------------------ presets

    pub fn presets(&self) -> Result<Vec<PresetRecord>> {
        let mut stmt = self
            .conn
            .prepare("SELECT source, target, uses FROM presets ORDER BY source COLLATE NOCASE")?;
        let rows = stmt.query_map([], |row| {
            Ok(PresetRecord {
                source: row.get(0)?,
                target: row.get(1)?,
                uses: row.get(2)?,
            })
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /// Insert or update a preset. Matching ignores case and leading/trailing
    /// whitespace so `" Tom Hanks "` and `"tom hanks"` are the same entry.
    /// An existing entry keeps its use counter.
    pub fn upsert_preset(&mut self, source: &str, target: &str) -> Result<()> {
        let source = source.trim();
        let target = target.trim();
        anyhow::ensure!(!source.is_empty(), "preset source must not be empty");
        anyhow::ensure!(!target.is_empty(), "preset target must not be empty");

        let tx = self.conn.transaction()?;
        let changed = tx.execute(
            "UPDATE presets SET source = ?1, target = ?2
             WHERE lower(trim(source)) = lower(trim(?1))",
            params![source, target],
        )?;
        if changed == 0 {
            tx.execute(
                "INSERT INTO presets (source, target, uses) VALUES (?1, ?2, 0)",
                params![source, target],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn delete_preset(&mut self, source: &str) -> Result<usize> {
        Ok(self.conn.execute(
            "DELETE FROM presets WHERE lower(trim(source)) = lower(trim(?1))",
            params![source],
        )?)
    }

    /// Count how often each preset name was actually applied.
    pub fn increment_preset_uses(&mut self, sources: &[String]) -> Result<()> {
        if sources.is_empty() {
            return Ok(());
        }
        let tx = self.conn.transaction()?;
        {
            let mut stmt = tx.prepare(
                "UPDATE presets SET uses = uses + 1
                 WHERE lower(trim(source)) = lower(trim(?1))",
            )?;
            for source in sources {
                stmt.execute([source])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    // ----------------------------------------------------------------- settings

    pub fn get_setting(&self, key: &str) -> Result<Option<String>> {
        let mut stmt = self
            .conn
            .prepare("SELECT value FROM settings WHERE key = ?1")?;
        let mut rows = stmt.query_map([key], |row| row.get::<_, String>(0))?;
        match rows.next() {
            Some(row) => Ok(Some(row?)),
            None => Ok(None),
        }
    }

    pub fn set_setting(&mut self, key: &str, value: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO settings (key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![key, value],
        )?;
        Ok(())
    }

    // ------------------------------------------------------------- translations

    pub fn upsert_translation(&mut self, record: &TranslationRecord) -> Result<()> {
        self.conn.execute(
            r#"
            INSERT INTO translations (item_id, field, lang, source_hash, text, provider, model,
                                      status, updated_at)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
            ON CONFLICT(item_id, field, lang) DO UPDATE SET
                source_hash = excluded.source_hash,
                text = excluded.text,
                provider = excluded.provider,
                model = excluded.model,
                status = excluded.status,
                updated_at = excluded.updated_at
            "#,
            params![
                record.item_id,
                record.field,
                record.lang,
                record.source_hash,
                record.text,
                record.provider,
                record.model,
                record.status,
                record.updated_at,
            ],
        )?;
        Ok(())
    }

    pub fn list_translations(&self, item_id: &str) -> Result<Vec<TranslationRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT item_id, field, lang, source_hash, text, provider, model, status, updated_at
             FROM translations WHERE item_id = ?1 ORDER BY field, lang",
        )?;
        let rows = stmt.query_map([item_id], |row| {
            Ok(TranslationRecord {
                item_id: row.get(0)?,
                field: row.get(1)?,
                lang: row.get(2)?,
                source_hash: row.get(3)?,
                text: row.get(4)?,
                provider: row.get(5)?,
                model: row.get(6)?,
                status: row.get(7)?,
                updated_at: row.get(8)?,
            })
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }
}

pub fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
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
            ..ItemRecord::default()
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
        assert_eq!(items[0].status, STATUS_UNTRANSLATED);
        assert!(!items[0].locked);

        // Upsert replaces, does not duplicate.
        store.upsert_items(&[sample()]).unwrap();
        assert_eq!(store.count_items().unwrap(), 1);
    }

    #[test]
    fn draft_status_and_lock_are_preserved_by_sync() {
        let mut store = Store::open_in_memory().unwrap();
        store.upsert_items(&[sample()]).unwrap();
        let draft = ItemDraft {
            name: "测试".into(),
            original_title: "测试原名".into(),
            overview: "简介".into(),
            tags: vec!["标签".into()],
            people: vec![PersonRecord {
                id: None,
                name: "演员".into(),
                role: Some("角色".into()),
                person_type: Some("Actor".into()),
            }],
        };
        store.save_draft("abc", &draft).unwrap();
        store.set_locked(&["abc".into()], true).unwrap();

        let item = store.get_item("abc").unwrap().unwrap();
        assert_eq!(item.status, STATUS_DRAFT);
        assert!(item.locked);
        assert_eq!(item.effective_name(), "测试");
        assert_eq!(item.effective_tags(), ["标签"]);
        assert_eq!(item.draft_original_title.as_deref(), Some("测试原名"));

        // A re-sync keeps draft/status/lock.
        store.upsert_items(&[sample()]).unwrap();
        let item = store.get_item("abc").unwrap().unwrap();
        assert_eq!(item.status, STATUS_DRAFT);
        assert!(item.locked);
        assert_eq!(item.draft_name.as_deref(), Some("测试"));
    }

    #[test]
    fn written_items_keep_the_original_source_snapshot_on_sync() {
        let mut store = Store::open_in_memory().unwrap();
        store.upsert_items(&[sample()]).unwrap();
        store
            .mark_written("abc", &["Name".into(), "Overview".into()])
            .unwrap();

        let mut translated = sample();
        translated.name = Some("测试".into());
        translated.overview = Some("简介".into());
        store.upsert_items(&[translated]).unwrap();

        let item = store.get_item("abc").unwrap().unwrap();
        assert_eq!(item.name.as_deref(), Some("Test"));
        assert_eq!(item.overview.as_deref(), Some("overview"));
        assert_eq!(item.status, STATUS_WRITTEN);
        assert_eq!(item.locked_fields, ["Name", "Overview"]);
    }

    #[test]
    fn libraries_and_counts() {
        let mut store = Store::open_in_memory().unwrap();
        store
            .upsert_libraries(&[
                LibraryRecord {
                    id: "lib".into(),
                    name: "Movies".into(),
                    collection_type: Some("movies".into()),
                },
                LibraryRecord {
                    id: "empty".into(),
                    name: "Empty".into(),
                    collection_type: None,
                },
            ])
            .unwrap();
        store.upsert_items(&[sample()]).unwrap();

        let libs = store.list_libraries().unwrap();
        assert_eq!(libs.len(), 2);
        assert_eq!(libs[0].name, "Empty");
        let counts = store.library_item_counts().unwrap();
        assert_eq!(counts.get("lib"), Some(&1));
        assert_eq!(counts.get("empty"), None);
    }

    #[test]
    fn presets_match_ignoring_case_and_space() {
        let mut store = Store::open_in_memory().unwrap();
        store.upsert_preset("Tom Hanks", "汤姆·汉克斯").unwrap();
        store.upsert_preset(" tom hanks ", "汤姆·汉克斯").unwrap();
        let presets = store.presets().unwrap();
        assert_eq!(presets.len(), 1);
        assert_eq!(presets[0].uses, 0);

        store.increment_preset_uses(&["TOM HANKS".into()]).unwrap();
        assert_eq!(store.presets().unwrap()[0].uses, 1);

        // Updating the target keeps the counter.
        store.upsert_preset("Tom Hanks", "汤姆汉克斯").unwrap();
        let presets = store.presets().unwrap();
        assert_eq!(presets[0].target, "汤姆汉克斯");
        assert_eq!(presets[0].uses, 1);

        store.delete_preset(" tom hanks").unwrap();
        assert!(store.presets().unwrap().is_empty());
    }

    #[test]
    fn settings_and_translations_roundtrip() {
        let mut store = Store::open_in_memory().unwrap();
        assert_eq!(store.get_setting("missing").unwrap(), None);
        store.set_setting("templates", r#"{"title":"x"}"#).unwrap();
        assert_eq!(
            store.get_setting("templates").unwrap().as_deref(),
            Some(r#"{"title":"x"}"#)
        );

        let record = TranslationRecord {
            item_id: "abc".into(),
            field: "name".into(),
            lang: "zh-CN".into(),
            source_hash: "hash".into(),
            text: "测试".into(),
            provider: "openai".into(),
            model: Some("model".into()),
            status: "draft".into(),
            updated_at: 7,
        };
        store.upsert_translation(&record).unwrap();
        store.upsert_translation(&record).unwrap();
        let rows = store.list_translations("abc").unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].text, "测试");
    }

    #[test]
    fn migrates_an_old_items_table_in_place() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("library.db");
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(
                r#"
                CREATE TABLE items (
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
                INSERT INTO items VALUES
                    ('old', 'lib', 'Movie', 'Old', NULL, NULL, '[]', '[]', NULL, '[]', 'h', 1);
                "#,
            )
            .unwrap();
        }

        let mut store = Store::open(&path).unwrap();
        let item = store.get_item("old").unwrap().unwrap();
        assert_eq!(item.status, STATUS_UNTRANSLATED);
        assert!(!item.locked);

        // The migrated store keeps working after another (idempotent) migrate.
        store.migrate().unwrap();
        store
            .save_draft(
                "old",
                &ItemDraft {
                    name: "旧".into(),
                    ..ItemDraft::default()
                },
            )
            .unwrap();
        assert_eq!(store.get_item("old").unwrap().unwrap().status, STATUS_DRAFT);
    }

    #[test]
    fn migration_is_idempotent() {
        let store = Store::open_in_memory().unwrap();
        store.migrate().unwrap();
        store.migrate().unwrap();
    }
}
