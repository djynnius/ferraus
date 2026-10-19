//! Small image containers and the image operations the OMR reconstruction needs:
//! grayscale pages, binary masks, Otsu thresholding, resizing, and connected components.

/// An 8-bit grayscale image (0 = black ink, 255 = white paper).
#[derive(Clone, Debug, PartialEq)]
pub struct Gray {
    pub w: usize,
    pub h: usize,
    pub px: Vec<u8>,
}

impl Gray {
    pub fn new(w: usize, h: usize, fill: u8) -> Gray {
        Gray { w, h, px: vec![fill; w * h] }
    }

    pub fn at(&self, x: usize, y: usize) -> u8 {
        self.px[y * self.w + x]
    }

    /// Resize with area averaging when shrinking and bilinear interpolation when growing.
    pub fn resize(&self, w: usize, h: usize) -> Gray {
        if w == self.w && h == self.h {
            return self.clone();
        }
        let img = image::GrayImage::from_raw(self.w as u32, self.h as u32, self.px.clone())
            .expect("gray buffer matches its size");
        let out = image::imageops::resize(&img, w as u32, h as u32, image::imageops::FilterType::Triangle);
        Gray { w, h, px: out.into_raw() }
    }

    /// Otsu's threshold over the histogram, clamped to a sane range for printed pages.
    pub fn otsu(&self) -> u8 {
        let mut hist = [0u64; 256];
        for &p in &self.px {
            hist[p as usize] += 1;
        }
        let total = self.px.len() as f64;
        let sum: f64 = hist.iter().enumerate().map(|(i, &c)| i as f64 * c as f64).sum();
        let (mut sum_b, mut w_b, mut best, mut best_t) = (0.0, 0.0, -1.0, 128u8);
        for (t, &c) in hist.iter().enumerate() {
            w_b += c as f64;
            if w_b == 0.0 {
                continue;
            }
            let w_f = total - w_b;
            if w_f == 0.0 {
                break;
            }
            sum_b += t as f64 * c as f64;
            let m_b = sum_b / w_b;
            let m_f = (sum - sum_b) / w_f;
            let between = w_b * w_f * (m_b - m_f) * (m_b - m_f);
            if between > best {
                best = between;
                best_t = t as u8;
            }
        }
        best_t.clamp(96, 200)
    }

    /// Ink mask: pixels darker than `threshold`.
    pub fn binarize(&self, threshold: u8) -> Mask {
        Mask { w: self.w, h: self.h, px: self.px.iter().map(|&p| (p < threshold) as u8).collect() }
    }
}

/// A binary mask (1 = set).
#[derive(Clone, Debug, PartialEq)]
pub struct Mask {
    pub w: usize,
    pub h: usize,
    pub px: Vec<u8>,
}

impl Mask {
    pub fn new(w: usize, h: usize) -> Mask {
        Mask { w, h, px: vec![0; w * h] }
    }

    #[inline]
    pub fn get(&self, x: usize, y: usize) -> bool {
        self.px[y * self.w + x] != 0
    }

    /// Like [`Mask::get`], false outside the image.
    #[inline]
    pub fn get_i(&self, x: i64, y: i64) -> bool {
        x >= 0 && y >= 0 && (x as usize) < self.w && (y as usize) < self.h && self.get(x as usize, y as usize)
    }

    #[inline]
    pub fn set(&mut self, x: usize, y: usize, v: bool) {
        self.px[y * self.w + x] = v as u8;
    }

    pub fn count(&self) -> usize {
        self.px.iter().filter(|&&p| p != 0).count()
    }

    /// Set pixels inside a box (clamped).
    pub fn count_in(&self, b: &BBox) -> usize {
        let (x0, y0, x1, y1) = b.clamp(self.w, self.h);
        let mut n = 0;
        for y in y0..y1 {
            n += self.px[y * self.w + x0..y * self.w + x1].iter().filter(|&&p| p != 0).count();
        }
        n
    }

    /// Fraction of set pixels inside a box.
    pub fn density(&self, b: &BBox) -> f64 {
        let (x0, y0, x1, y1) = b.clamp(self.w, self.h);
        let area = (x1.saturating_sub(x0) * y1.saturating_sub(y0)).max(1);
        self.count_in(b) as f64 / area as f64
    }

    /// Length of the vertical run of set pixels through `(x, y)` going in direction `dir`
    /// (-1 up, +1 down), bridging gaps of at most `gap` pixels.
    pub fn run_from(&self, x: i64, y: i64, dir: i64, gap: i64) -> i64 {
        let mut len = 0;
        let mut miss = 0;
        let mut yy = y;
        loop {
            yy += dir;
            if yy < 0 || yy >= self.h as i64 {
                break;
            }
            if self.get_i(x, yy) {
                len = (yy - y).abs();
                miss = 0;
            } else {
                miss += 1;
                if miss > gap {
                    break;
                }
            }
        }
        len
    }

    /// Connected components (8-connectivity), each with its box and pixel count.
    pub fn components(&self) -> Vec<Component> {
        self.components_in(&BBox::new(0, 0, self.w as i64, self.h as i64))
    }

    /// Connected components restricted to a box.
    pub fn components_in(&self, area: &BBox) -> Vec<Component> {
        let (ax0, ay0, ax1, ay1) = area.clamp(self.w, self.h);
        let (aw, ah) = (ax1.saturating_sub(ax0), ay1.saturating_sub(ay0));
        let mut label = vec![0u32; aw * ah];
        let mut out = Vec::new();
        let mut stack: Vec<(usize, usize)> = Vec::new();
        for sy in 0..ah {
            for sx in 0..aw {
                if label[sy * aw + sx] != 0 || !self.get(ax0 + sx, ay0 + sy) {
                    continue;
                }
                let id = out.len() as u32 + 1;
                let mut c = Component {
                    bbox: BBox::new((ax0 + sx) as i64, (ay0 + sy) as i64, (ax0 + sx + 1) as i64, (ay0 + sy + 1) as i64),
                    area: 0,
                    pixels: Vec::new(),
                };
                label[sy * aw + sx] = id;
                stack.push((sx, sy));
                while let Some((x, y)) = stack.pop() {
                    c.area += 1;
                    c.pixels.push(((ax0 + x) as u32, (ay0 + y) as u32));
                    c.bbox.include((ax0 + x) as i64, (ay0 + y) as i64);
                    for dy in -1i64..=1 {
                        for dx in -1i64..=1 {
                            let (nx, ny) = (x as i64 + dx, y as i64 + dy);
                            if nx < 0 || ny < 0 || nx >= aw as i64 || ny >= ah as i64 {
                                continue;
                            }
                            let (nx, ny) = (nx as usize, ny as usize);
                            if label[ny * aw + nx] == 0 && self.get(ax0 + nx, ay0 + ny) {
                                label[ny * aw + nx] = id;
                                stack.push((nx, ny));
                            }
                        }
                    }
                }
                out.push(c);
            }
        }
        out
    }
}

/// An axis-aligned box, `x1`/`y1` exclusive.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BBox {
    pub x0: i64,
    pub y0: i64,
    pub x1: i64,
    pub y1: i64,
}

impl BBox {
    pub fn new(x0: i64, y0: i64, x1: i64, y1: i64) -> BBox {
        BBox { x0, y0, x1, y1 }
    }
    pub fn w(&self) -> i64 {
        self.x1 - self.x0
    }
    pub fn h(&self) -> i64 {
        self.y1 - self.y0
    }
    pub fn cx(&self) -> f64 {
        (self.x0 + self.x1) as f64 / 2.0
    }
    pub fn cy(&self) -> f64 {
        (self.y0 + self.y1) as f64 / 2.0
    }
    pub fn include(&mut self, x: i64, y: i64) {
        self.x0 = self.x0.min(x);
        self.y0 = self.y0.min(y);
        self.x1 = self.x1.max(x + 1);
        self.y1 = self.y1.max(y + 1);
    }
    pub fn union(&self, o: &BBox) -> BBox {
        BBox::new(self.x0.min(o.x0), self.y0.min(o.y0), self.x1.max(o.x1), self.y1.max(o.y1))
    }
    pub fn grow(&self, dx: i64, dy: i64) -> BBox {
        BBox::new(self.x0 - dx, self.y0 - dy, self.x1 + dx, self.y1 + dy)
    }
    pub fn overlaps(&self, o: &BBox) -> bool {
        self.x0 < o.x1 && o.x0 < self.x1 && self.y0 < o.y1 && o.y0 < self.y1
    }
    pub fn x_overlap(&self, o: &BBox) -> i64 {
        (self.x1.min(o.x1) - self.x0.max(o.x0)).max(0)
    }
    pub fn clamp(&self, w: usize, h: usize) -> (usize, usize, usize, usize) {
        let c = |v: i64, m: usize| v.clamp(0, m as i64) as usize;
        (c(self.x0, w), c(self.y0, h), c(self.x1, w), c(self.y1, h))
    }
}

/// A connected component of a mask.
#[derive(Clone, Debug, PartialEq)]
pub struct Component {
    pub bbox: BBox,
    pub area: usize,
    pub pixels: Vec<(u32, u32)>,
}

impl Component {
    /// The component as a mask the size of its box.
    pub fn local_mask(&self) -> Mask {
        let b = self.bbox;
        let mut m = Mask::new(b.w() as usize, b.h() as usize);
        for &(x, y) in &self.pixels {
            m.set((x as i64 - b.x0) as usize, (y as i64 - b.y0) as usize, true);
        }
        m
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn components_and_runs() {
        let mut m = Mask::new(10, 10);
        for y in 1..8 {
            m.set(2, y, true);
        }
        m.set(6, 6, true);
        m.set(7, 7, true);
        let cs = m.components();
        assert_eq!(cs.len(), 2);
        assert_eq!(cs[0].bbox, BBox::new(2, 1, 3, 8));
        assert_eq!(cs[1].area, 2);
        assert_eq!(m.run_from(2, 7, -1, 0), 6);
        assert_eq!(m.run_from(2, 1, 1, 0), 6);
        assert!((m.density(&BBox::new(2, 1, 3, 8)) - 1.0).abs() < 1e-9);
    }

    #[test]
    fn otsu_splits_ink_from_paper() {
        let mut g = Gray::new(20, 20, 250);
        for i in 0..100 {
            g.px[i] = 10;
        }
        let t = g.otsu();
        assert!(t > 10 && t < 250);
        assert_eq!(g.binarize(t).count(), 100);
    }
}
