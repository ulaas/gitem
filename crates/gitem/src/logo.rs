//! The gem-and-branch logo as plain geometry. No dependencies: the build script uses
//! this too, to give the executable its icon.

pub type Point = (f32, f32);
pub type Rgb = (u8, u8, u8);

/// A cut gem in a unit square, back to front. Fixed colours: it's the same mark in
/// every theme.
pub const FACETS: [(&[Point], Rgb); 6] = [
    (&[(0.22, 0.12), (0.38, 0.40), (0.02, 0.40)], (0x55, 0x98, 0xe7)),
    (&[(0.22, 0.12), (0.78, 0.12), (0.62, 0.40), (0.38, 0.40)], (0x86, 0xb6, 0xef)),
    (&[(0.78, 0.12), (0.98, 0.40), (0.62, 0.40)], (0x2a, 0x78, 0xd6)),
    (&[(0.02, 0.40), (0.38, 0.40), (0.50, 0.94)], (0x2a, 0x78, 0xd6)),
    (&[(0.38, 0.40), (0.62, 0.40), (0.50, 0.94)], (0x55, 0x98, 0xe7)),
    (&[(0.62, 0.40), (0.98, 0.40), (0.50, 0.94)], (0x1c, 0x5c, 0xab)),
];
/// A branch cut into the stone: two commits on a line and one forking off.
pub const NODES: [Point; 3] = [(0.50, 0.27), (0.50, 0.70), (0.72, 0.33)];
pub const NODE_RADIUS: f32 = 0.065;
pub const EDGE_WIDTH: f32 = 0.05;
pub const EDGES: [(Point, Point); 2] = [(NODES[0], NODES[1]), ((0.50, 0.55), NODES[2])];

fn in_polygon(points: &[Point], x: f32, y: f32) -> bool {
    let mut inside = false;
    let mut j = points.len() - 1;
    for i in 0..points.len() {
        let (a, b) = (points[i], points[j]);
        if (a.1 > y) != (b.1 > y) && x < (b.0 - a.0) * (y - a.1) / (b.1 - a.1) + a.0 {
            inside = !inside;
        }
        j = i;
    }
    inside
}

fn segment_distance(a: Point, b: Point, x: f32, y: f32) -> f32 {
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let t = (((x - a.0) * dx + (y - a.1) * dy) / (dx * dx + dy * dy)).clamp(0.0, 1.0);
    ((x - a.0 - t * dx).powi(2) + (y - a.1 - t * dy).powi(2)).sqrt()
}

fn sample(x: f32, y: f32) -> Option<Rgb> {
    let on_edge = EDGES.iter().any(|(a, b)| segment_distance(*a, *b, x, y) <= EDGE_WIDTH / 2.0);
    let on_node = NODES.iter().any(|n| ((x - n.0).powi(2) + (y - n.1).powi(2)).sqrt() <= NODE_RADIUS);
    if on_edge || on_node {
        return Some((255, 255, 255));
    }
    FACETS.iter().rev().find(|(p, _)| in_polygon(p, x, y)).map(|(_, c)| *c)
}

/// The logo as straight RGBA, top row first, antialiased by supersampling.
pub fn rgba(size: u32) -> Vec<u8> {
    const SAMPLES: u32 = 4;
    let mut out = Vec::with_capacity((size * size * 4) as usize);
    for py in 0..size {
        for px in 0..size {
            let (mut r, mut g, mut b, mut hits) = (0u32, 0u32, 0u32, 0u32);
            for sy in 0..SAMPLES {
                for sx in 0..SAMPLES {
                    let x = (px as f32 + (sx as f32 + 0.5) / SAMPLES as f32) / size as f32;
                    let y = (py as f32 + (sy as f32 + 0.5) / SAMPLES as f32) / size as f32;
                    if let Some(c) = sample(x, y) {
                        r += c.0 as u32;
                        g += c.1 as u32;
                        b += c.2 as u32;
                        hits += 1;
                    }
                }
            }
            let div = hits.max(1);
            out.extend_from_slice(&[(r / div) as u8, (g / div) as u8, (b / div) as u8, (hits * 255 / (SAMPLES * SAMPLES)) as u8]);
        }
    }
    out
}
