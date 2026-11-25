//! Shrinking a picture one row at a time, as it is decoded, so that a big
//! picture is never held whole: the sum of the pixels that fall on each pixel
//! of the result, weighted by their alpha, and one row of sums at a time.

/// Where a row of the source goes and how big the result is.
pub struct Downscaler {
    sw: usize,
    sh: usize,
    dw: usize,
    dh: usize,
    /// Source columns that fall on each result column.
    columns: Vec<u32>,
    /// Sums of r*a, g*a, b*a and a, for the result row being filled.
    sums: Vec<u32>,
    current: usize,
    rows_in_current: u32,
    next_source_row: usize,
    out: Vec<u8>,
}

/// The size a `width` by `height` picture is shrunk to so that its longer
/// side is at most `limit`. A picture that fits is not changed.
pub fn target_size(width: usize, height: usize, limit: usize) -> (usize, usize) {
    let longest = width.max(height);
    if longest <= limit {
        return (width, height);
    }
    let shrink =
        |side: usize| ((side as u64 * limit as u64).div_ceil(longest as u64)).max(1) as usize;
    (shrink(width), shrink(height))
}

impl Downscaler {
    pub fn new(sw: usize, sh: usize, limit: usize) -> Downscaler {
        let (dw, dh) = target_size(sw, sh, limit);
        let mut columns = vec![0u32; dw];
        for x in 0..sw {
            columns[x * dw / sw] += 1;
        }
        Downscaler {
            sw,
            sh,
            dw,
            dh,
            columns,
            sums: vec![0; dw * 4],
            current: 0,
            rows_in_current: 0,
            next_source_row: 0,
            out: Vec::with_capacity(dw * dh * 4),
        }
    }

    /// The size of the result.
    pub fn size(&self) -> (usize, usize) {
        (self.dw, self.dh)
    }

    /// The next row of the source, as RGBA with straight alpha.
    pub fn push_row(&mut self, row: &[u8]) {
        debug_assert_eq!(row.len(), self.sw * 4);
        if self.next_source_row >= self.sh {
            return;
        }
        let target = self.next_source_row * self.dh / self.sh;
        self.next_source_row += 1;
        if target != self.current {
            self.flush();
            self.current = target;
        }
        self.rows_in_current += 1;
        for (x, pixel) in row.as_chunks::<4>().0.iter().enumerate() {
            let sum = &mut self.sums[x * self.dw / self.sw * 4..][..4];
            let alpha = u32::from(pixel[3]);
            sum[0] += u32::from(pixel[0]) * alpha;
            sum[1] += u32::from(pixel[1]) * alpha;
            sum[2] += u32::from(pixel[2]) * alpha;
            sum[3] += alpha;
        }
    }

    fn flush(&mut self) {
        for (column, sum) in self
            .columns
            .iter()
            .zip(self.sums.as_chunks_mut::<4>().0.iter_mut())
        {
            let alpha = sum[3];
            if alpha == 0 {
                self.out.extend_from_slice(&[0, 0, 0, 0]);
            } else {
                let half = alpha / 2;
                for channel in &sum[..3] {
                    self.out.push(((channel + half) / alpha) as u8);
                }
                let covered = (column * self.rows_in_current).max(1);
                self.out.push(((alpha + covered / 2) / covered) as u8);
            }
            sum.fill(0);
        }
        self.rows_in_current = 0;
    }

    /// The result: RGBA rows, `size()` pixels. A source that ended early
    /// leaves the missing rows transparent.
    pub fn finish(mut self) -> Vec<u8> {
        if self.rows_in_current > 0 {
            self.flush();
        }
        self.out.resize(self.dw * self.dh * 4, 0);
        self.out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(
        sw: usize,
        sh: usize,
        limit: usize,
        pixel: impl Fn(usize, usize) -> [u8; 4],
    ) -> (usize, usize, Vec<u8>) {
        let mut scaler = Downscaler::new(sw, sh, limit);
        for y in 0..sh {
            let row: Vec<u8> = (0..sw).flat_map(|x| pixel(x, y)).collect();
            scaler.push_row(&row);
        }
        let (w, h) = scaler.size();
        (w, h, scaler.finish())
    }

    #[test]
    fn a_picture_that_fits_is_unchanged() {
        let (w, h, out) = run(5, 3, 8, |x, y| [x as u8, y as u8, 7, 255]);
        assert_eq!((w, h), (5, 3));
        assert_eq!(&out[..4], &[0, 0, 7, 255]);
        assert_eq!(&out[(2 * 5 + 4) * 4..][..4], &[4, 2, 7, 255]);
    }

    #[test]
    fn the_longer_side_is_the_limit_and_the_shape_is_kept() {
        assert_eq!(target_size(6000, 4000, 3000), (3000, 2000));
        assert_eq!(target_size(1000, 4000, 1000), (250, 1000));
        assert_eq!(target_size(100_000, 1, 3072), (3072, 1));
        assert_eq!(target_size(10, 10, 3072), (10, 10));
    }

    #[test]
    fn pixels_are_averaged() {
        // Black and white columns shrunk two to one are grey.
        let (w, h, out) = run(4, 2, 2, |x, _| {
            if x % 2 == 0 {
                [0, 0, 0, 255]
            } else {
                [255, 255, 255, 255]
            }
        });
        assert_eq!((w, h), (2, 1));
        assert_eq!(&out[..4], &[128, 128, 128, 255]);
        assert_eq!(&out[4..8], &[128, 128, 128, 255]);
    }

    #[test]
    fn clear_pixels_do_not_darken_the_colour_beside_them() {
        let (_, _, out) = run(2, 1, 1, |x, _| {
            if x == 0 {
                [200, 100, 50, 255]
            } else {
                [0, 0, 0, 0]
            }
        });
        assert_eq!(&out[..3], &[200, 100, 50]);
        assert_eq!(out[3], 128);
    }

    #[test]
    fn a_short_source_leaves_the_rest_clear() {
        let mut scaler = Downscaler::new(2, 4, 100);
        scaler.push_row(&[9; 8]);
        let out = scaler.finish();
        assert_eq!(out.len(), 2 * 4 * 4);
        assert_eq!(&out[..4], &[9, 9, 9, 9]);
        assert!(out[8..].iter().all(|b| *b == 0));
    }
}
