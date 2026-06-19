//! Embed path for trie. Non-eponymous vtab; takes
//! `source=NAME, key_column=COL, case_insensitive=BOOL` args at
//! CREATE VIRTUAL TABLE time. Trie is built lazily on first
//! filter against the source table via `exec_query`.

use alloc::boxed::Box;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::cell::RefCell;
use core::ffi::c_int;
use std::collections::HashMap;
use sqlite_embed::{
    exec_query, register_vtabs, BestIndexInfo, SqlValueOwned, VtabSpec,
};

const COL_WORD:   i32 = 0;
const COL_PREFIX: i32 = 1;

const SQLITE_INDEX_CONSTRAINT_EQ: u8 = 2;

struct TrieNode {
    children: HashMap<char, Box<TrieNode>>,
    terminal: Option<String>,
}

impl TrieNode {
    fn new() -> Self {
        Self {
            children: HashMap::new(),
            terminal: None,
        }
    }
    fn insert(&mut self, word: &str) {
        let mut cur = self;
        for ch in word.chars() {
            cur = cur
                .children
                .entry(ch)
                .or_insert_with(|| Box::new(TrieNode::new()));
        }
        cur.terminal = Some(word.to_string());
    }
    fn descend(&self, prefix: &str) -> Option<&TrieNode> {
        let mut cur = self;
        for ch in prefix.chars() {
            cur = cur.children.get(&ch)?;
        }
        Some(cur)
    }
    fn collect_into(&self, out: &mut Vec<String>) {
        if let Some(w) = &self.terminal {
            out.push(w.clone());
        }
        for child in self.children.values() {
            child.collect_into(out);
        }
    }
}

struct TrieVtab {
    db: *mut libsqlite3_sys::sqlite3,
    source: String,
    key_column: String,
    case_insensitive: bool,
    /// Built lazily on first filter; cleared on disconnect.
    cache: RefCell<Option<Box<TrieNode>>>,
}

struct TrieCursor {
    vtab: *const TrieVtab,
    matches: Vec<String>,
    idx: usize,
}

fn strip_quotes(s: &str) -> &str {
    let s = s
        .strip_prefix('\'')
        .and_then(|s| s.strip_suffix('\''))
        .unwrap_or(s);
    s.strip_prefix('"').and_then(|s| s.strip_suffix('"')).unwrap_or(s)
}

unsafe fn tr_make_vtab(
    _table_name: &str,
    args: &[&str],
    db: *mut libsqlite3_sys::sqlite3,
) -> Result<*mut (), String> {
    let mut source: Option<String> = None;
    let mut key_column: Option<String> = None;
    let mut case_insensitive = false;
    for arg in args {
        let (k, v) = arg
            .split_once('=')
            .ok_or_else(|| format!("trie: arg {arg:?} not key=value"))?;
        let v = strip_quotes(v.trim());
        match k.trim() {
            "source" => source = Some(v.to_string()),
            "key_column" => key_column = Some(v.to_string()),
            "case_insensitive" => {
                case_insensitive = matches!(v, "1" | "true" | "yes")
            }
            other => return Err(format!("trie: unknown arg {other:?}")),
        }
    }
    let source = source.ok_or_else(|| "trie: source= is required".to_string())?;
    let key_column =
        key_column.ok_or_else(|| "trie: key_column= is required".to_string())?;
    let v = Box::new(TrieVtab {
        db,
        source,
        key_column,
        case_insensitive,
        cache: RefCell::new(None),
    });
    Ok(Box::into_raw(v) as *mut ())
}

unsafe fn tr_destroy_vtab(state: *mut ()) {
    drop(Box::from_raw(state as *mut TrieVtab));
}

unsafe fn tr_best_index(_state: *mut (), info: &mut BestIndexInfo) -> Result<(), String> {
    let mut bound = false;
    for (i, c) in info.constraints.iter().enumerate() {
        if !c.usable || c.column != COL_PREFIX || c.op != SQLITE_INDEX_CONSTRAINT_EQ {
            continue;
        }
        if bound {
            continue;
        }
        bound = true;
        info.usage[i].argv_index = 1;
        info.usage[i].omit = true;
    }
    info.idx_num = if bound { 1 } else { 0 };
    info.estimated_cost = if bound { 10.0 } else { 1.0e18 };
    info.estimated_rows = 100;
    Ok(())
}

unsafe fn tr_make_cursor(
    vtab_state: *mut (),
    _db: *mut libsqlite3_sys::sqlite3,
) -> *mut () {
    Box::into_raw(Box::new(TrieCursor {
        vtab: vtab_state as *const TrieVtab,
        matches: Vec::new(),
        idx: 0,
    })) as *mut ()
}

unsafe fn tr_destroy_cursor(state: *mut ()) {
    drop(Box::from_raw(state as *mut TrieCursor));
}

unsafe fn ensure_built(vtab: &TrieVtab) -> Result<(), String> {
    if vtab.cache.borrow().is_some() {
        return Ok(());
    }
    let sql = format!(
        "SELECT {key} FROM {src}",
        key = vtab.key_column,
        src = vtab.source,
    );
    let rows = exec_query(vtab.db, &sql, &[])
        .map_err(|e| format!("trie: scan source: {e}"))?;
    let mut root = Box::new(TrieNode::new());
    for row in &rows {
        if let Some(SqlValueOwned::Text(word)) = row.first() {
            let w = if vtab.case_insensitive {
                word.to_lowercase()
            } else {
                word.clone()
            };
            root.insert(&w);
        }
    }
    *vtab.cache.borrow_mut() = Some(root);
    Ok(())
}

unsafe fn tr_filter(
    cursor: *mut (),
    idx_num: i32,
    _idx_str: Option<&str>,
    args: &[SqlValueOwned],
) -> Result<(), String> {
    let c = &mut *(cursor as *mut TrieCursor);
    c.matches.clear();
    c.idx = 0;
    let prefix = if idx_num & 1 != 0 {
        match args.first() {
            Some(SqlValueOwned::Text(s)) => s.clone(),
            _ => String::new(),
        }
    } else {
        String::new()
    };
    let vtab = &*c.vtab;
    ensure_built(vtab)?;
    let p = if vtab.case_insensitive {
        prefix.to_lowercase()
    } else {
        prefix
    };
    let cache = vtab.cache.borrow();
    if let Some(root) = cache.as_ref() {
        if let Some(node) = root.descend(&p) {
            node.collect_into(&mut c.matches);
        }
    }
    c.matches.sort();
    Ok(())
}

unsafe fn tr_next(state: *mut ()) -> Result<(), String> {
    (*(state as *mut TrieCursor)).idx += 1;
    Ok(())
}

unsafe fn tr_eof(state: *mut ()) -> bool {
    let c = &*(state as *const TrieCursor);
    c.idx >= c.matches.len()
}

unsafe fn tr_column(state: *mut (), col: i32) -> Result<SqlValueOwned, String> {
    let c = &*(state as *const TrieCursor);
    match (col, c.matches.get(c.idx).cloned()) {
        (COL_WORD, Some(w)) => Ok(SqlValueOwned::Text(w)),
        (COL_WORD, None) => Ok(SqlValueOwned::Null),
        (COL_PREFIX, _) => Ok(SqlValueOwned::Null),
        (other, _) => Err(format!("trie: bad column {other}")),
    }
}

unsafe fn tr_rowid(state: *mut ()) -> Result<i64, String> {
    Ok(((*(state as *const TrieCursor)).idx + 1) as i64)
}

const VTABS: &[VtabSpec] = &[VtabSpec {
    name: b"trie\0",
    schema: b"CREATE TABLE x(word TEXT, prefix TEXT HIDDEN)\0",
    eponymous: false,
    make_vtab: tr_make_vtab,
    destroy_vtab: tr_destroy_vtab,
    best_index: tr_best_index,
    make_cursor: tr_make_cursor,
    destroy_cursor: tr_destroy_cursor,
    filter: tr_filter,
    next: tr_next,
    eof: tr_eof,
    column: tr_column,
    update: None,
    begin: None,
    sync: None,
    commit: None,
    rollback: None,
    rename: None,
    savepoint: None,
    release: None,
    rollback_to: None,
    rowid: tr_rowid,
}];

pub unsafe fn register_into(db: *mut libsqlite3_sys::sqlite3) -> c_int {
    register_vtabs(db, VTABS)
}
