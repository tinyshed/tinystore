//! What names a key, an owner or a folder in every engine: text, or an
//! integer spelled in decimal.

use std::borrow::Cow;

/// What names a key, an owner or a folder: text, or an integer spelled in
/// decimal, so that `42` and `"42"` are one key.
pub trait Key {
    fn text(&self) -> Cow<'_, str>;
}

impl Key for str {
    fn text(&self) -> Cow<'_, str> {
        Cow::Borrowed(self)
    }
}

impl Key for String {
    fn text(&self) -> Cow<'_, str> {
        Cow::Borrowed(self)
    }
}

impl<K: Key + ?Sized> Key for &K {
    fn text(&self) -> Cow<'_, str> {
        (**self).text()
    }
}

macro_rules! integer_keys {
    ($($integer:ty),*) => {
        $(impl Key for $integer {
            fn text(&self) -> Cow<'_, str> {
                Cow::Owned(self.to_string())
            }
        })*
    };
}

integer_keys!(i8, i16, i32, i64, i128, isize, u8, u16, u32, u64, u128, usize);
