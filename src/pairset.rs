use std::{
    hash::*,
    ops::{Deref, DerefMut},
};

use crate::pairmap::PairMap;

/// A wrapper around [PairMap]
#[derive(Debug)]
pub struct PairSet<T, U = T> {
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
    pub fn new() -> Self {
        Self {
            inner: PairMap::new(),
        }
    }

    pub fn insert(&mut self, left: T, right: U) -> bool
    where
        T: Hash + Eq,
        U: Hash + Eq,
    {
        self.inner.insert(left, right, ()).is_none()
    }
}
