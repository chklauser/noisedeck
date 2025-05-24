pub struct PadIter<I>
where
    I: Iterator,
{
    inner: Option<I>,
    min_len: usize,
    pad: I::Item,
}

pub struct PadAlternateIter<'a, I1, I2>
where
    I1: Iterator,
    I2: Iterator<Item = I1::Item>,
{
    inner: Option<I1>,
    alternate: Option<I2>,
    min_len: usize,
    inner_cnt: Option<&'a mut usize>,
}

pub trait IterExt<I>
where
    I: Iterator,
{
    fn pad(self, min_len: usize, pad: I::Item) -> PadIter<I>;

    fn pad_alt_cnt<I2>(
        self,
        min_len: usize,
        alt: I2,
        inner_cnt: &mut usize,
    ) -> PadAlternateIter<I, I2>
    where
        I2: Iterator<Item = I::Item>;
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

    fn pad_alt_cnt<I2>(
        self,
        min_len: usize,
        alt: I2,
        inner_cnt: &mut usize,
    ) -> PadAlternateIter<I, I2>
    where
        I2: Iterator<Item = I::Item>,
    {
        PadAlternateIter {
            inner: Some(self),
            alternate: Some(alt),
            min_len,
            inner_cnt: Some(inner_cnt),
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

impl<I1, I2> Iterator for PadAlternateIter<'_, I1, I2>
where
    I1: Iterator,
    I2: Iterator<Item = I1::Item>,
{
    type Item = I1::Item;

    fn next(&mut self) -> Option<Self::Item> {
        if let Some(inner) = &mut self.inner {
            if let Some(v) = inner.next() {
                self.min_len = self.min_len.saturating_sub(1);
                if let Some(inner_cnt) = &mut self.inner_cnt {
                    **inner_cnt += 1;
                }
                return Some(v);
            } else {
                self.inner = None;
            }
        }
        if self.min_len > 0 {
            if let Some(alt) = &mut self.alternate {
                if let Some(v) = alt.next() {
                    self.min_len = self.min_len.saturating_sub(1);
                    Some(v)
                } else {
                    self.alternate = None;
                    None
                }
            } else {
                None
            }
        } else {
            None
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let inner = self.inner.as_ref().map(|inner| inner.size_hint());
        let inner_min = inner.map(|(min, _)| min).unwrap_or(0);
        let inner_max = inner.and_then(|(_, max)| max);
        let alt = self.alternate.as_ref().map(|alt| alt.size_hint());
        let alt_min = alt.map(|(min, _)| min).unwrap_or(0);
        let alt_max = alt.and_then(|(_, max)| max);

        (
            inner_min + alt_min.max(self.min_len),
            inner_max
                .map(|imax| imax.max(alt_max.unwrap_or(alt_min).min(self.min_len)))
                .or(alt_max.map(|amax| amax.min(self.min_len))),
        )
    }
}
