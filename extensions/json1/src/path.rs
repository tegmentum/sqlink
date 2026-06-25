//! SQLite json1 path parser + tree walker.
//!
//! Path grammar (per https://sqlite.org/json1.html#jpath):
//!   path     := '$' (segment)*
//!   segment  := '.' identifier | '.' quoted-identifier | '[' index ']'
//!   identifier := \w+
//!   quoted-identifier := '"' [^"]* '"'
//!   index    := -?\d+        (negative counts from end of array)
//!
//! We only need a small subset of features for the scalar
//! functions: traverse to a value, optionally creating intermediates
//! (json_set / json_insert) and detect "does the position exist?"
//! (json_replace).

use alloc::string::String;
use alloc::vec::Vec;
use core::fmt;

use serde_json::Value;

#[derive(Debug, Clone)]
pub enum Segment {
    Key(String),
    Index(i64),
}

#[derive(Debug)]
pub enum PathError {
    Empty,
    MissingDollar,
    BadIndex(String),
    UnterminatedQuote,
    UnexpectedChar(char),
}

impl fmt::Display for PathError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PathError::Empty => write!(f, "empty path"),
            PathError::MissingDollar => write!(f, "path must start with '$'"),
            PathError::BadIndex(s) => write!(f, "bad index: {s}"),
            PathError::UnterminatedQuote => write!(f, "unterminated quoted identifier"),
            PathError::UnexpectedChar(c) => write!(f, "unexpected char: {c}"),
        }
    }
}

pub fn parse(path: &str) -> Result<Vec<Segment>, PathError> {
    let mut chars = path.chars().peekable();
    match chars.next() {
        None => return Err(PathError::Empty),
        Some('$') => {}
        Some(_) => return Err(PathError::MissingDollar),
    }
    let mut out = Vec::new();
    while let Some(&c) = chars.peek() {
        match c {
            '.' => {
                chars.next();
                // Either quoted ".\"key\"" or bare ".key".
                if chars.peek() == Some(&'"') {
                    chars.next();
                    let mut s = String::new();
                    let mut closed = false;
                    for ch in chars.by_ref() {
                        if ch == '"' {
                            closed = true;
                            break;
                        }
                        s.push(ch);
                    }
                    if !closed {
                        return Err(PathError::UnterminatedQuote);
                    }
                    out.push(Segment::Key(s));
                } else {
                    let mut s = String::new();
                    while let Some(&ch) = chars.peek() {
                        if ch == '.' || ch == '[' {
                            break;
                        }
                        s.push(ch);
                        chars.next();
                    }
                    out.push(Segment::Key(s));
                }
            }
            '[' => {
                chars.next();
                let mut s = String::new();
                let mut closed = false;
                for ch in chars.by_ref() {
                    if ch == ']' {
                        closed = true;
                        break;
                    }
                    s.push(ch);
                }
                if !closed {
                    return Err(PathError::UnexpectedChar('['));
                }
                let idx = s.parse::<i64>().map_err(|_| PathError::BadIndex(s))?;
                out.push(Segment::Index(idx));
            }
            _ => return Err(PathError::UnexpectedChar(c)),
        }
    }
    Ok(out)
}

/// Resolve a parsed path against a value. Returns None if any
/// segment misses (key missing, index OOB, type mismatch).
pub fn resolve<'a>(root: &'a Value, segs: &[Segment]) -> Option<&'a Value> {
    let mut cur = root;
    for seg in segs {
        cur = match seg {
            Segment::Key(k) => cur.as_object()?.get(k)?,
            Segment::Index(i) => {
                let arr = cur.as_array()?;
                let idx = normalize_index(*i, arr.len())?;
                arr.get(idx)?
            }
        };
    }
    Some(cur)
}

/// Mutable variant. Returns the slot at the path or None on miss.
#[allow(dead_code)]
pub fn resolve_mut<'a>(root: &'a mut Value, segs: &[Segment]) -> Option<&'a mut Value> {
    let mut cur = root;
    for seg in segs {
        cur = match seg {
            Segment::Key(k) => cur.as_object_mut()?.get_mut(k)?,
            Segment::Index(i) => {
                let arr = cur.as_array_mut()?;
                let idx = normalize_index(*i, arr.len())?;
                arr.get_mut(idx)?
            }
        };
    }
    Some(cur)
}

/// Set a value at path, creating intermediate objects as needed.
/// Mirrors json_set's behavior. Returns Ok(()) if the path was
/// settable (which it almost always is for valid path shapes).
///
/// `if_present_only` = true rejects when the leaf doesn't already
/// exist (json_replace).
/// `if_missing_only` = true rejects when the leaf already exists
/// (json_insert).
pub fn set(
    root: &mut Value,
    segs: &[Segment],
    new_val: Value,
    if_present_only: bool,
    if_missing_only: bool,
) -> Result<(), &'static str> {
    if segs.is_empty() {
        if if_present_only || if_missing_only {
            // json_replace($, ...) replaces, json_insert($, ...) errors.
            if if_missing_only {
                return Err("cannot insert at root");
            }
        }
        *root = new_val;
        return Ok(());
    }
    // Walk all intermediates, coercing each parent into the
    // container shape its next segment expects.
    let mut cur = root;
    for window in segs.windows(2) {
        coerce_for(cur, &window[1]);
        cur = step_mut_or_create(cur, &window[0])?;
    }
    // Coerce the final parent into the shape the last segment
    // expects (e.g. `set($.a.b, 7)` on `{}` walks to the slot for
    // "a" which we just created as Null and now needs to become
    // an object holding "b").
    let last = segs.last().unwrap();
    coerce_for(cur, last);
    match last {
        Segment::Key(k) => {
            let obj = cur.as_object_mut().ok_or("parent is not an object")?;
            let exists = obj.contains_key(k);
            if if_missing_only && exists {
                return Ok(());
            }
            if if_present_only && !exists {
                return Ok(());
            }
            obj.insert(k.clone(), new_val);
        }
        Segment::Index(i) => {
            let arr = cur.as_array_mut().ok_or("parent is not an array")?;
            match normalize_index(*i, arr.len()) {
                Some(idx) => {
                    if if_missing_only {
                        return Ok(());
                    }
                    arr[idx] = new_val;
                }
                None => {
                    if if_present_only {
                        return Ok(());
                    }
                    // Append (mirrors json_set/json_insert append-on-miss).
                    arr.push(new_val);
                }
            }
        }
    }
    Ok(())
}

/// Coerce `cur` into the container type required by the next
/// segment: Key -> Object, Index -> Array. No-op when already
/// the right type. Used by `set` to traverse through Nulls
/// created during the walk.
fn coerce_for(cur: &mut Value, next: &Segment) {
    match next {
        Segment::Key(_) if !cur.is_object() => {
            *cur = Value::Object(serde_json::Map::new());
        }
        Segment::Index(_) if !cur.is_array() => {
            *cur = Value::Array(Vec::new());
        }
        _ => {}
    }
}

/// Remove the value at the path. No-op on miss (matches json_remove
/// semantics: silently drops non-existent paths).
pub fn remove(root: &mut Value, segs: &[Segment]) -> Result<(), &'static str> {
    if segs.is_empty() {
        // json_remove($) clears to null.
        *root = Value::Null;
        return Ok(());
    }
    let (last, init) = segs.split_last().unwrap();
    let mut cur = root;
    for seg in init {
        match step_mut(cur, seg) {
            Some(next) => cur = next,
            None => return Ok(()),
        }
    }
    match last {
        Segment::Key(k) => {
            if let Some(obj) = cur.as_object_mut() {
                obj.remove(k);
            }
        }
        Segment::Index(i) => {
            if let Some(arr) = cur.as_array_mut() {
                if let Some(idx) = normalize_index(*i, arr.len()) {
                    arr.remove(idx);
                }
            }
        }
    }
    Ok(())
}

fn step_mut<'a>(cur: &'a mut Value, seg: &Segment) -> Option<&'a mut Value> {
    match seg {
        Segment::Key(k) => cur.as_object_mut()?.get_mut(k),
        Segment::Index(i) => {
            let arr = cur.as_array_mut()?;
            let idx = normalize_index(*i, arr.len())?;
            arr.get_mut(idx)
        }
    }
}

fn step_mut_or_create<'a>(
    cur: &'a mut Value,
    seg: &Segment,
) -> Result<&'a mut Value, &'static str> {
    match seg {
        Segment::Key(k) => {
            if !cur.is_object() {
                *cur = Value::Object(serde_json::Map::new());
            }
            let obj = cur.as_object_mut().unwrap();
            Ok(obj.entry(k.clone()).or_insert(Value::Null))
        }
        Segment::Index(i) => {
            if !cur.is_array() {
                *cur = Value::Array(Vec::new());
            }
            let arr = cur.as_array_mut().unwrap();
            match normalize_index(*i, arr.len()) {
                Some(idx) => Ok(arr.get_mut(idx).unwrap()),
                None => {
                    arr.push(Value::Null);
                    let last = arr.len() - 1;
                    Ok(arr.get_mut(last).unwrap())
                }
            }
        }
    }
}

fn normalize_index(i: i64, len: usize) -> Option<usize> {
    if i >= 0 {
        let idx = i as usize;
        if idx < len {
            Some(idx)
        } else {
            None
        }
    } else {
        let abs = (-i) as usize;
        if abs <= len {
            Some(len - abs)
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_root() {
        assert_eq!(parse("$").unwrap().len(), 0);
    }

    #[test]
    fn parses_key_chain() {
        let segs = parse("$.a.b").unwrap();
        assert_eq!(segs.len(), 2);
        assert!(matches!(&segs[0], Segment::Key(s) if s == "a"));
        assert!(matches!(&segs[1], Segment::Key(s) if s == "b"));
    }

    #[test]
    fn parses_index() {
        let segs = parse("$[0]").unwrap();
        assert!(matches!(&segs[0], Segment::Index(0)));
    }

    #[test]
    fn parses_negative_index() {
        let segs = parse("$[-1]").unwrap();
        assert!(matches!(&segs[0], Segment::Index(-1)));
    }

    #[test]
    fn parses_quoted_identifier() {
        let segs = parse("$.\"weird key\"").unwrap();
        assert!(matches!(&segs[0], Segment::Key(s) if s == "weird key"));
    }

    #[test]
    fn parses_mixed() {
        let segs = parse("$.a[0].b").unwrap();
        assert_eq!(segs.len(), 3);
    }

    #[test]
    fn resolves_nested() {
        let v = json!({"a": {"b": [1, 2, 3]}});
        let segs = parse("$.a.b[1]").unwrap();
        assert_eq!(resolve(&v, &segs), Some(&json!(2)));
    }

    #[test]
    fn resolves_negative_index() {
        let v = json!([10, 20, 30]);
        let segs = parse("$[-1]").unwrap();
        assert_eq!(resolve(&v, &segs), Some(&json!(30)));
    }

    #[test]
    fn miss_returns_none() {
        let v = json!({"a": 1});
        assert!(resolve(&v, &parse("$.b").unwrap()).is_none());
    }

    #[test]
    fn set_creates_intermediate_objects() {
        let mut v = json!({});
        set(&mut v, &parse("$.a.b").unwrap(), json!(7), false, false).unwrap();
        assert_eq!(v, json!({"a": {"b": 7}}));
    }

    #[test]
    fn set_replace_only_skips_when_absent() {
        let mut v = json!({"a": 1});
        set(&mut v, &parse("$.b").unwrap(), json!(2), true, false).unwrap();
        assert_eq!(v, json!({"a": 1}));
    }

    #[test]
    fn set_insert_only_skips_when_present() {
        let mut v = json!({"a": 1});
        set(&mut v, &parse("$.a").unwrap(), json!(2), false, true).unwrap();
        assert_eq!(v, json!({"a": 1}));
    }

    #[test]
    fn remove_drops_key() {
        let mut v = json!({"a": 1, "b": 2});
        remove(&mut v, &parse("$.a").unwrap()).unwrap();
        assert_eq!(v, json!({"b": 2}));
    }

    #[test]
    fn remove_drops_array_element() {
        let mut v = json!([10, 20, 30]);
        remove(&mut v, &parse("$[1]").unwrap()).unwrap();
        assert_eq!(v, json!([10, 30]));
    }

    #[test]
    fn remove_missing_is_noop() {
        let mut v = json!({"a": 1});
        remove(&mut v, &parse("$.b").unwrap()).unwrap();
        assert_eq!(v, json!({"a": 1}));
    }
}
