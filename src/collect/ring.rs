use std::collections::VecDeque;

/// Bounded ring buffer for sparkline history.
#[derive(Debug, Clone)]
pub struct Ring<T> {
    cap: usize,
    inner: VecDeque<T>,
}

impl<T: Clone> Ring<T> {
    pub fn new(cap: usize) -> Self {
        Self { cap, inner: VecDeque::with_capacity(cap) }
    }
    pub fn push(&mut self, v: T) {
        if self.inner.len() == self.cap {
            self.inner.pop_front();
        }
        self.inner.push_back(v);
    }
    pub fn iter(&self) -> impl Iterator<Item = &T> {
        self.inner.iter()
    }
    pub fn len(&self) -> usize {
        self.inner.len()
    }
    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }
    pub fn last(&self) -> Option<&T> {
        self.inner.back()
    }
    pub fn to_vec(&self) -> Vec<T> {
        self.inner.iter().cloned().collect()
    }
}
