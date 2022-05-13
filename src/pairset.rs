//! A wrapper around `PairMap` which is itself a wrapper for
//! [hashbrown::HashMap] to accomodate the limitations of the Borrow trait with
//! tuples until it will be GATified

use std::{
	hash::*,
	ops::{Deref, DerefMut},
};

use crate::pairmap::PairMap;

/// A wrapper around [PairMap]
#[derive(Debug)]
pub struct PairSet<T, U = T> {
	/// the struct we're wrapping
	inner: PairMap<T, U, ()>,
}

impl<T, U> Deref for PairSet<T, U> {
	type Target = PairMap<T, U, ()>;

	fn deref(&self) -> &Self::Target {
		&self.inner
	}
}

impl<T, U> DerefMut for PairSet<T, U> {
	fn deref_mut(&mut self) -> &mut Self::Target {
		&mut self.inner
	}
}

impl<T, U> PairSet<T, U> {
	/// Constructs a new `PairSet`
	pub fn new() -> Self {
		Self { inner: PairMap::new() }
	}

	/// Adds a value pair to the set.
	pub fn insert(&mut self, left: T, right: U) -> bool
	where
		T: Hash + Eq,
		U: Hash + Eq,
	{
		self.inner.insert(left, right, ()).is_none()
	}
}
