pub struct PadIter<I>
where
    I: Iterator,
{
    inner: Option<I>,
    min_len: usize,
    pad: I::Item,
}

pub trait IterExt<I>
where
    I: Iterator,
{
    fn pad(self, min_len: usize, pad: I::Item) -> PadIter<I>;
}

impl<I> IterExt<I> for I
where
    I: Iterator,
{
    fn pad(self, min_len: usize, pad: I::Item) -> PadIter<I> {
        PadIter {
            inner: Some(self),
            min_len,
            pad,
        }
    }
}

impl<I> Iterator for PadIter<I>
where
    I: Iterator,
    I::Item: Clone,
{
    type Item = I::Item;

    fn next(&mut self) -> Option<Self::Item> {
        if let Some(inner) = &mut self.inner {
            if let Some(v) = inner.next() {
                self.min_len -= 1;
                return Some(v);
            } else {
                self.inner = None;
            }
        }
        if self.min_len > 0 {
            self.min_len -= 1;
            Some(self.pad.clone())
        } else {
            None
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        if let Some(inner) = &self.inner {
            let (min, max) = inner.size_hint();
            (min.max(self.min_len), max.map(|max| max.max(self.min_len)))
        } else {
            (self.min_len, Some(self.min_len))
        }
    }
}
