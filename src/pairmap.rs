use std::{borrow::Borrow, hash::*};

use hashbrown::{hash_map::RawEntryMut, HashMap};

/// A wrapper around [hashbrown::HashMap] because it's impossible to implement Borrow<(&Left, &Right)> for (Left, Right) so we have to do unnecessary clones for every [HashSet::get][hashbrown::HashSet::get], [HashMap::get_mut][hashbrown::HashMap::get_mut] call
#[derive(Debug)]
pub struct PairMap<T, U, V> {
    inner: HashMap<(T, U), V>,
}

impl<T, U, V> PairMap<T, U, V> {
    pub fn new() -> Self {
        Self {
            inner: HashMap::new(),
        }
    }
}

impl<T, U, V> PairMap<T, U, V> {
    pub fn insert(&mut self, left: T, right: U, v: V) -> Option<V>
    where
        T: Hash + Eq,
        U: Hash + Eq,
    {
        self.inner.insert((left, right), v)
    }

    pub fn get<Q, R>(&self, left: &Q, right: &R) -> Option<&V>
    where
        T: Borrow<Q>,
        U: Borrow<R>,
        Q: ?Sized + Hash + Eq,
        R: ?Sized + Hash + Eq,
    {
        let mut hasher = self.inner.hasher().build_hasher();
        (left, right).hash(&mut hasher);
        let hash = hasher.finish();
        self.inner
            .raw_entry()
            .from_hash(hash, |(l, r)| (l.borrow(), r.borrow()) == (left, right))
            .map(|(_, v)| v)
    }

    pub fn get_mut<Q, R>(&mut self, left: &Q, right: &R) -> Option<&mut V>
    where
        T: Borrow<Q>,
        U: Borrow<R>,
        Q: ?Sized + Hash + Eq,
        R: ?Sized + Hash + Eq,
    {
        let mut hasher = self.inner.hasher().build_hasher();
        (left, right).hash(&mut hasher);
        let hash = hasher.finish();
        let entry = self
            .inner
            .raw_entry_mut()
            .from_hash(hash, |(l, r)| (l.borrow(), r.borrow()) == (left, right));

        match entry {
            RawEntryMut::Occupied(entry) => Some(entry.into_mut()),
            RawEntryMut::Vacant(_) => None,
        }
    }

    pub fn remove<Q, R>(&mut self, left: &Q, right: &R) -> Option<V>
    where
        T: Borrow<Q>,
        U: Borrow<R>,
        Q: ?Sized + Hash + Eq,
        R: ?Sized + Hash + Eq,
    {
        let mut hasher = self.inner.hasher().build_hasher();
        (left, right).hash(&mut hasher);
        let hash = hasher.finish();
        let entry = self
            .inner
            .raw_entry_mut()
            .from_hash(hash, |(l, r)| (l.borrow(), r.borrow()) == (left, right));
        match entry {
            RawEntryMut::Occupied(entry) => Some(entry.remove_entry().1),
            RawEntryMut::Vacant(_) => None,
        }
    }

    pub fn contains<Q, R>(&self, left: &Q, right: &R) -> bool
    where
        T: Borrow<Q>,
        U: Borrow<R>,
        Q: ?Sized + Hash + Eq,
        R: ?Sized + Hash + Eq,
    {
        self.get(left, right).is_some()
    }
}
