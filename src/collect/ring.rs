use std::collections::VecDeque;

/// Bounded ring buffer for sparkline history.
#[derive(Debug, Clone)]
pub struct Ring<T> {
    cap: usize,
    inner: VecDeque<T>,
}

impl<T: Clone> Ring<T> {
    pub fn new(cap: usize) -> Self {
        Self {
            cap,
            inner: VecDeque::with_capacity(cap),
        }
    }
    pub fn push(&mut self, v: T) {
        if self.inner.len() == self.cap {
            self.inner.pop_front();
        }
        self.inner.push_back(v);
    }
    #[allow(dead_code)]
    pub fn iter(&self) -> impl Iterator<Item = &T> {
        self.inner.iter()
    }
    pub fn len(&self) -> usize {
        self.inner.len()
    }
    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }
    #[allow(dead_code)]
    pub fn last(&self) -> Option<&T> {
        self.inner.back()
    }
    /// Get the nth element counting back from the most recent (0 = newest).
    pub fn nth_back(&self, n: usize) -> Option<&T> {
        let len = self.inner.len();
        if n >= len {
            return None;
        }
        self.inner.get(len - 1 - n)
    }
    pub fn to_vec(&self) -> Vec<T> {
        self.inner.iter().cloned().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_ring() {
        let r: Ring<u32> = Ring::new(4);
        assert!(r.is_empty());
        assert_eq!(r.len(), 0);
        assert_eq!(r.last(), None);
        assert_eq!(r.nth_back(0), None);
    }

    #[test]
    fn fills_to_capacity() {
        let mut r: Ring<u32> = Ring::new(3);
        r.push(1);
        r.push(2);
        r.push(3);
        assert_eq!(r.len(), 3);
        assert_eq!(r.to_vec(), vec![1, 2, 3]);
        assert_eq!(r.last(), Some(&3));
    }

    #[test]
    fn pushes_beyond_cap_drop_oldest() {
        let mut r: Ring<u32> = Ring::new(3);
        for v in 1..=10 {
            r.push(v);
        }
        // Cap is 3, so we keep the last three.
        assert_eq!(r.len(), 3);
        assert_eq!(r.to_vec(), vec![8, 9, 10]);
        assert_eq!(r.last(), Some(&10));
    }

    #[test]
    fn nth_back_zero_is_newest() {
        let mut r: Ring<u32> = Ring::new(5);
        r.push(10);
        r.push(20);
        r.push(30);
        assert_eq!(r.nth_back(0), Some(&30));
        assert_eq!(r.nth_back(1), Some(&20));
        assert_eq!(r.nth_back(2), Some(&10));
    }

    #[test]
    fn nth_back_past_end_is_none() {
        let mut r: Ring<u32> = Ring::new(5);
        r.push(1);
        r.push(2);
        assert_eq!(r.nth_back(2), None);
        assert_eq!(r.nth_back(99), None);
    }

    #[test]
    fn nth_back_after_wrap_indexes_from_newest() {
        let mut r: Ring<u32> = Ring::new(3);
        for v in 1..=10 {
            r.push(v);
        }
        // After wrap the contents are [8, 9, 10]; nth_back walks backward.
        assert_eq!(r.nth_back(0), Some(&10));
        assert_eq!(r.nth_back(2), Some(&8));
        assert_eq!(r.nth_back(3), None);
    }
}
