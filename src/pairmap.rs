//! a wrapper around [hashbrown::HashMap] to accomodate for the limitations of
//! the current `Borrow` trait. This could be also solved by a GATified version
//! of the `Borrow` trait in std
use std::{borrow::Borrow, hash::*};

use hashbrown::{hash_map::RawEntryMut, HashMap};

/// A wrapper around [hashbrown::HashMap] because it's impossible to implement
/// Borrow<(&Left, &Right)> for (Left, Right) so we have to do unnecessary
/// clones for every [HashSet::get][hashbrown::HashSet::get],
/// [HashMap::get_mut][hashbrown::HashMap::get_mut] call
///
/// For the Borrow trait to work with references to tuple elements it needs to
/// be GATified
#[derive(Debug)]
pub struct PairMap<T, U, V> {
	/// the map object we're wrapping
	inner: HashMap<(T, U), V>,
}

impl<T, U, V> PairMap<T, U, V> {
	/// Creates an empty `PairMap`
	pub fn new() -> Self {
		Self { inner: HashMap::new() }
	}
}

impl<T, U, V> PairMap<T, U, V> {
	/// Inserts a key-value pair into the `PairMap`
	pub fn insert(&mut self, left: T, right: U, v: V) -> Option<V>
	where
		T: Hash + Eq,
		U: Hash + Eq,
	{
		self.inner.insert((left, right), v)
	}

	/// Returns a reference to the value corresponding to the key pair
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

	/// Returns a mutable reference to the value corresponding to the key pair
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

	/// Removes a key pair from the map, returning the value at the key if the
	/// key was previously in the map.
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

	/// Returns true if the map contains a value for the specified key pair
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
