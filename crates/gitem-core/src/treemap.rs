//! Squarified treemap layout, independent of any UI.

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Cell {
    pub index: usize,
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

/// Lay out `values` in the given rectangle. Each area is proportional to its
/// value, and rows are chosen to keep cells as square as possible.
pub fn layout(values: &[f64], x: f32, y: f32, w: f32, h: f32) -> Vec<Cell> {
    let total: f64 = values.iter().sum();
    if total <= 0.0 || w <= 0.0 || h <= 0.0 {
        return Vec::new();
    }
    let scale = (w as f64 * h as f64) / total;
    let mut order: Vec<usize> = (0..values.len()).filter(|&i| values[i] > 0.0).collect();
    order.sort_by(|&a, &b| values[b].total_cmp(&values[a]));
    let mut out = Vec::with_capacity(order.len());
    let (mut rx, mut ry, mut rw, mut rh) = (x as f64, y as f64, w as f64, h as f64);
    let mut i = 0;
    while i < order.len() {
        let side = rw.min(rh);
        let mut sum = 0.0;
        let mut best = f64::MAX;
        let mut end = i;
        while end < order.len() {
            let a = values[order[end]] * scale;
            let s = sum + a;
            let lo = (values[order[end]] * scale).min(values[order[i]] * scale);
            let hi = values[order[i]] * scale;
            let worst = (side * side * hi / (s * s)).max(s * s / (side * side * lo));
            if end > i && worst > best {
                break;
            }
            best = worst;
            sum = s;
            end += 1;
        }
        let thick = sum / side;
        let mut pos = 0.0;
        for &k in &order[i..end] {
            let len = values[k] * scale / thick;
            if rw >= rh {
                out.push(Cell { index: k, x: rx as f32, y: (ry + pos) as f32, w: thick as f32, h: len as f32 });
            } else {
                out.push(Cell { index: k, x: (rx + pos) as f32, y: ry as f32, w: len as f32, h: thick as f32 });
            }
            pos += len;
        }
        if rw >= rh {
            rx += thick;
            rw -= thick;
        } else {
            ry += thick;
            rh -= thick;
        }
        i = end;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn areas_are_proportional_and_inside() {
        let v = [6.0, 6.0, 4.0, 3.0, 2.0, 2.0, 1.0];
        let c = layout(&v, 10.0, 20.0, 600.0, 400.0);
        assert_eq!(c.len(), 7);
        let total: f64 = v.iter().sum();
        for cell in &c {
            let want = v[cell.index] / total * 240_000.0;
            assert!(((cell.w * cell.h) as f64 - want).abs() < 2.0, "{cell:?}");
            assert!(cell.x >= 9.9 && cell.y >= 19.9 && cell.x + cell.w <= 610.1 && cell.y + cell.h <= 420.1, "{cell:?}");
        }
        // No two cells overlap.
        for a in &c {
            for b in &c {
                if a.index != b.index {
                    let apart = a.x + a.w <= b.x + 0.1 || b.x + b.w <= a.x + 0.1 || a.y + a.h <= b.y + 0.1 || b.y + b.h <= a.y + 0.1;
                    assert!(apart, "{a:?} {b:?}");
                }
            }
        }
    }

    #[test]
    fn edge_cases() {
        assert!(layout(&[], 0.0, 0.0, 10.0, 10.0).is_empty());
        assert!(layout(&[0.0], 0.0, 0.0, 10.0, 10.0).is_empty());
        assert_eq!(layout(&[1.0], 0.0, 0.0, 10.0, 10.0).len(), 1);
    }
}
