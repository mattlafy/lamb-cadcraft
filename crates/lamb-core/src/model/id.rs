//! Document ids: an opaque `u64` (CADCraft hands out `Handle(u64)`), with a scoped string-id
//! interner for reading LAMB's string-id JSON (golden fixtures today, `.lamb` files later).
//!
//! Outside [`with_string_ids`], an [`Id`] serializes as a JSON number and deserializes only from a
//! number. Inside it, a string deserializes by interning (first sight gets the next id, counting
//! from [`INTERNED_BASE`]), and serializing gives the original string back; an id the interner
//! never saw serializes as `lamb-{n:08x}-0`, the shape of a TS `generateId` result.

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::fmt;

use serde::de::{self, Visitor};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// An entity, view, level or link id. Layer ids are not `Id`s: layers are keyed by name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Id(pub u64);

/// First id handed to an interned string, so interned ids never collide with the small ids a
/// port mints itself.
pub const INTERNED_BASE: u64 = 1 << 48;

/// The original string of each interned id, keyed by id.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct IdStrings(pub BTreeMap<u64, String>);

#[derive(Default)]
struct Interner {
    by_str: HashMap<String, u64>,
    by_id: HashMap<u64, String>,
    /// One past the largest id held, or 0 when empty.
    next_after_max: u64,
}

impl Interner {
    fn seeded(seed: &IdStrings) -> Interner {
        let mut int = Interner::default();
        for (&n, s) in &seed.0 {
            int.insert(s, n);
        }
        int
    }

    fn insert(&mut self, s: &str, n: u64) {
        self.by_str.insert(s.to_owned(), n);
        self.by_id.insert(n, s.to_owned());
        self.next_after_max = self.next_after_max.max(n + 1);
    }

    fn intern(&mut self, s: &str) -> u64 {
        if let Some(&n) = self.by_str.get(s) {
            return n;
        }
        let n = (INTERNED_BASE + self.by_str.len() as u64).max(self.next_after_max);
        self.insert(s, n);
        n
    }

    fn table(&self) -> IdStrings {
        IdStrings(self.by_id.iter().map(|(&n, s)| (n, s.clone())).collect())
    }
}

thread_local! {
    static INTERNER: RefCell<Option<Interner>> = const { RefCell::new(None) };
}

/// Restores the previous interner state when dropped, so a panic inside `f` can't leak one.
struct Restore(Option<Interner>);

impl Drop for Restore {
    fn drop(&mut self) {
        let prev = self.0.take();
        INTERNER.with(|i| *i.borrow_mut() = prev);
    }
}

/// Runs `f` with a fresh string-id interner installed on this thread (see the module docs),
/// restoring whatever was installed before afterwards.
pub fn with_string_ids<R>(f: impl FnOnce() -> R) -> R {
    with_string_ids_seeded(&IdStrings::default(), f).0
}

/// Like [`with_string_ids`], but the interner starts pre-filled from `seed`, and the table it ends
/// with (`seed` plus everything interned inside `f`) is returned alongside `f`'s result. A new
/// string gets an id past every seeded one, so a sparse seed cannot collide.
pub fn with_string_ids_seeded<R>(seed: &IdStrings, f: impl FnOnce() -> R) -> (R, IdStrings) {
    let prev = INTERNER.with(|i| i.borrow_mut().replace(Interner::seeded(seed)));
    let _restore = Restore(prev);
    let r = f();
    let table = INTERNER.with(|i| i.borrow().as_ref().map(Interner::table).unwrap_or_default());
    (r, table)
}

impl Id {
    /// The string form inside [`with_string_ids`]; `None` outside it.
    fn scoped_string(self) -> Option<String> {
        INTERNER.with(|i| i.borrow().as_ref().map(|int| int.by_id.get(&self.0).cloned().unwrap_or_else(|| format!("lamb-{:08x}-0", self.0))))
    }
}

/// Inside [`with_string_ids`], the same string serialization gives (the original string, or
/// `lamb-{n:08x}-0` for an id the interner never saw); outside it, the decimal number. Use this for
/// string keys built from ids.
impl fmt::Display for Id {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.scoped_string() {
            Some(t) => f.write_str(&t),
            None => write!(f, "{}", self.0),
        }
    }
}

impl Serialize for Id {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self.scoped_string() {
            Some(t) => s.serialize_str(&t),
            None => s.serialize_u64(self.0),
        }
    }
}

struct IdVisitor;

impl Visitor<'_> for IdVisitor {
    type Value = Id;

    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("a non-negative integer id (or a string id inside with_string_ids)")
    }

    fn visit_u64<E: de::Error>(self, v: u64) -> Result<Id, E> {
        Ok(Id(v))
    }

    fn visit_i64<E: de::Error>(self, v: i64) -> Result<Id, E> {
        u64::try_from(v).map(Id).map_err(|_| E::custom(format!("negative id {v}")))
    }

    fn visit_str<E: de::Error>(self, v: &str) -> Result<Id, E> {
        INTERNER
            .with(|i| i.borrow_mut().as_mut().map(|int| int.intern(v)))
            .map(Id)
            .ok_or_else(|| E::custom(format!("string id {v:?} outside with_string_ids")))
    }
}

impl<'de> Deserialize<'de> for Id {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Id, D::Error> {
        d.deserialize_any(IdVisitor)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn number_round_trip_outside_interner() {
        let id: Id = serde_json::from_value(json!(42)).unwrap();
        assert_eq!(id, Id(42));
        assert_eq!(serde_json::to_value(id).unwrap(), json!(42));
        assert!(serde_json::from_value::<Id>(json!("view-1")).is_err());
        assert!(serde_json::from_value::<Id>(json!(-1)).is_err());
    }

    #[test]
    fn string_round_trip_inside_interner() {
        with_string_ids(|| {
            let ids: Vec<Id> = serde_json::from_value(json!(["a", "b", "a", 7])).unwrap();
            assert_eq!(ids, vec![Id(INTERNED_BASE), Id(INTERNED_BASE + 1), Id(INTERNED_BASE), Id(7)]);
            assert_eq!(serde_json::to_value(&ids[..3]).unwrap(), json!(["a", "b", "a"]));
        });
        // Gone afterwards.
        assert!(serde_json::from_value::<Id>(json!("a")).is_err());
    }

    #[test]
    fn unseen_id_serializes_as_minted_shape() {
        with_string_ids(|| {
            assert_eq!(serde_json::to_value(Id(7)).unwrap(), json!("lamb-00000007-0"));
        });
    }

    #[test]
    fn display_interned_minted_and_unscoped() {
        assert_eq!(Id(42).to_string(), "42");
        with_string_ids(|| {
            let a: Id = serde_json::from_value(json!("view-1")).unwrap();
            assert_eq!(a.to_string(), "view-1");
            assert_eq!(Id(7).to_string(), "lamb-00000007-0");
            assert_eq!(format!("{}:{}", a, Id(255)), "view-1:lamb-000000ff-0");
        });
        assert_eq!(Id(INTERNED_BASE).to_string(), INTERNED_BASE.to_string());
    }

    #[test]
    fn nested_scopes_restore_outer_interner() {
        with_string_ids(|| {
            let a: Id = serde_json::from_value(json!("outer")).unwrap();
            with_string_ids(|| {
                assert_eq!(serde_json::to_value(a).unwrap(), json!(format!("lamb-{:08x}-0", a.0)));
            });
            assert_eq!(serde_json::to_value(a).unwrap(), json!("outer"));
        });
    }

    #[test]
    fn seeded_scope_serializes_seed_and_interns_past_it() {
        let seed = IdStrings(BTreeMap::from([(INTERNED_BASE, "plan-1".to_owned())]));
        let ((), table) = with_string_ids_seeded(&seed, || {
            assert_eq!(serde_json::to_value(Id(INTERNED_BASE)).unwrap(), json!("plan-1"));
            let again: Id = serde_json::from_value(json!("plan-1")).unwrap();
            assert_eq!(again, Id(INTERNED_BASE));
            let b: Id = serde_json::from_value(json!("b")).unwrap();
            assert_eq!(b, Id(INTERNED_BASE + 1));
        });
        assert_eq!(table, IdStrings(BTreeMap::from([(INTERNED_BASE, "plan-1".to_owned()), (INTERNED_BASE + 1, "b".to_owned())])));
    }

    #[test]
    fn sparse_seed_cannot_collide() {
        let seed = IdStrings(BTreeMap::from([(INTERNED_BASE + 5, "x".to_owned())]));
        let (id, table) = with_string_ids_seeded(&seed, || serde_json::from_value::<Id>(json!("y")).unwrap());
        assert_eq!(id, Id(INTERNED_BASE + 6));
        assert_eq!(table.0.len(), 2);
        assert_eq!(table.0.get(&(INTERNED_BASE + 5)).map(String::as_str), Some("x"));
    }

    #[test]
    fn nested_seeded_scope_restores_outer_interner() {
        with_string_ids(|| {
            let a: Id = serde_json::from_value(json!("outer")).unwrap();
            let seed = IdStrings(BTreeMap::from([(INTERNED_BASE + 9, "inner".to_owned())]));
            let ((), _) = with_string_ids_seeded(&seed, || {
                assert_eq!(serde_json::to_value(Id(INTERNED_BASE + 9)).unwrap(), json!("inner"));
            });
            assert_eq!(serde_json::to_value(a).unwrap(), json!("outer"));
            assert_eq!(serde_json::to_value(Id(INTERNED_BASE + 9)).unwrap(), json!(format!("lamb-{:08x}-0", INTERNED_BASE + 9)));
        });
    }
}
