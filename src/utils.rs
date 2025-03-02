use std::hash::{BuildHasher, Hasher};

/// Pass a u64 value straight through as the hash. This is only useful if you
/// are pre-hashing a `HashMap` key and don't want to do duplicate the work.
pub struct NoopHasher {
    inner: u64,
}

impl Hasher for NoopHasher {
    fn finish(&self) -> u64 {
        self.inner
    }

    fn write(&mut self, _bytes: &[u8]) {
        debug_assert!(
            false,
            "NoopHasher only supports u64s that were already hashed"
        )
    }

    fn write_u64(&mut self, i: u64) {
        self.inner = i;
    }
}

/// An implementation of [`BuildHasher`] that just passes a u64 key straight
/// through.
#[derive(Default, Debug, Clone, Copy)]
pub struct BuildNoopHasher;

impl BuildHasher for BuildNoopHasher {
    type Hasher = NoopHasher;

    fn build_hasher(&self) -> Self::Hasher {
        NoopHasher { inner: 0 }
    }
}
