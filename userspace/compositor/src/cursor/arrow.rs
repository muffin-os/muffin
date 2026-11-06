//! The arrow cursor has two triangles at a hotspot. The outer triangle is
//! the outline. The inner triangle is `OUTLINE_PX` inside it and is the fill.
//! The pressed buttons select the paints.

use gfx::api::Rect;

use crate::pointer::Buttons;

/// These are the vertices of the arrow in pixels, relative to the hotspot.
/// The first vertex is the hotspot. The second vertex is the lowest, and the
/// third vertex is the rightmost.
const SHAPE: [[f32; 2]; 3] = [[0.0, 0.0], [0.0, 19.0], [13.0, 13.0]];
/// The size includes the pixel of the hotspot and the pixels up to the
/// farthest vertex.
const SHAPE_WIDTH: u32 = SHAPE[2][0] as u32 + 1;
const SHAPE_HEIGHT: u32 = SHAPE[1][1] as u32 + 1;
/// This is the distance between the edges of the outer and the inner triangle.
const OUTLINE_PX: f32 = 1.5;

/// The arrow has two triangles of three vertices.
pub const VERTEX_COUNT: usize = 6;

/// This pixel is inside the fill, relative to the hotspot.
const FILL_PROBE: (u32, u32) = (4, 10);
/// This pixel is on the outline, relative to the hotspot.
const OUTLINE_PROBE: (u32, u32) = (0, 10);

/// This colour has channels in the range `0.0..=1.0`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rgb(pub [f32; 3]);

impl Rgb {
    /// Returns true when an `0xAARRGGBB` pixel shows this colour.
    pub fn matches(self, pixel: u32) -> bool {
        /// This is the permitted difference of each channel, out of 255. The
        /// rounding to 8 bits causes a difference of 1. The remaining tolerance is
        /// for the colour pipeline of the GPU.
        const TOLERANCE: i32 = 8;
        let channels = [(pixel >> 16) & 0xFF, (pixel >> 8) & 0xFF, pixel & 0xFF];
        self.0
            .iter()
            .zip(channels)
            .all(|(want, got)| ((want * 255.0).round() as i32 - got as i32).abs() <= TOLERANCE)
    }
}

/// This is the role that the fill paints. The left button has priority over
/// the right button.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fill {
    Idle,
    Primary,
    Secondary,
}

impl Fill {
    pub fn of(buttons: Buttons) -> Self {
        if buttons.left {
            Self::Primary
        } else if buttons.right {
            Self::Secondary
        } else {
            Self::Idle
        }
    }

    pub fn rgb(self) -> Rgb {
        match self {
            Self::Idle => Rgb([1.0, 1.0, 1.0]),
            Self::Primary => Rgb([0.20, 0.50, 0.95]),
            Self::Secondary => Rgb([0.20, 0.80, 0.40]),
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Primary => "primary",
            Self::Secondary => "secondary",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outline {
    Idle,
    Middle,
}

impl Outline {
    pub fn of(buttons: Buttons) -> Self {
        if buttons.middle {
            Self::Middle
        } else {
            Self::Idle
        }
    }

    pub fn rgb(self) -> Rgb {
        match self {
            Self::Idle => Rgb([0.0, 0.0, 0.0]),
            Self::Middle => Rgb([0.90, 0.10, 0.10]),
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Middle => "middle",
        }
    }
}

/// This is one vertex of the cursor. The backend packs it into its own layout.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Vertex {
    /// The position is in clip space. Both axes go from `-1.0` to `1.0`, and
    /// `y` points up.
    pub clip: [f32; 2],
    pub rgb: Rgb,
}

pub struct Arrow {
    screen_width: u32,
    screen_height: u32,
}

impl Arrow {
    pub fn new(screen_width: u32, screen_height: u32) -> Self {
        Self {
            screen_width,
            screen_height,
        }
    }

    /// Returns the pixels that the arrow covers with its hotspot at `(x, y)`.
    /// The screen edge cuts the result. The result is empty when the hotspot is
    /// off the screen.
    pub fn bounds(&self, (x, y): (u32, u32)) -> Rect {
        Rect {
            x,
            y,
            width: SHAPE_WIDTH.min(self.screen_width.saturating_sub(x)),
            height: SHAPE_HEIGHT.min(self.screen_height.saturating_sub(y)),
        }
    }

    /// Returns a pixel inside the fill of the arrow with its hotspot at `(x, y)`.
    pub fn fill_probe(&self, (x, y): (u32, u32)) -> (u32, u32) {
        (x + FILL_PROBE.0, y + FILL_PROBE.1)
    }

    /// Returns a pixel on the outline of the arrow with its hotspot at `(x, y)`.
    pub fn outline_probe(&self, (x, y): (u32, u32)) -> (u32, u32) {
        (x + OUTLINE_PROBE.0, y + OUTLINE_PROBE.1)
    }

    /// Returns the outline triangle and then the fill triangle, with the
    /// hotspot at `(x, y)` and the paints of `buttons`.
    pub fn vertices(&self, (x, y): (u32, u32), buttons: Buttons) -> [Vertex; VERTEX_COUNT] {
        let outer = SHAPE.map(|[dx, dy]| [x as f32 + dx, y as f32 + dy]);
        let inner = inset(outer, OUTLINE_PX);
        let to_clip = |[px, py]: [f32; 2]| {
            [
                px / self.screen_width as f32 * 2.0 - 1.0,
                1.0 - py / self.screen_height as f32 * 2.0,
            ]
        };
        let vertex = |rgb: Rgb| {
            move |point: [f32; 2]| Vertex {
                clip: to_clip(point),
                rgb,
            }
        };
        let [o0, o1, o2] = outer.map(vertex(Outline::of(buttons).rgb()));
        let [i0, i1, i2] = inner.map(vertex(Fill::of(buttons).rgb()));
        [o0, o1, o2, i0, i1, i2]
    }
}

/// Returns the triangle with edges that are `distance` inside the edges of
/// `outer`. Each vertex moves towards the incentre. Its distance to the
/// incentre scales by `(inradius - distance) / inradius`.
fn inset(outer: [[f32; 2]; 3], distance: f32) -> [[f32; 2]; 3] {
    let [a, b, c] = outer;
    let length = |p: [f32; 2], q: [f32; 2]| ((p[0] - q[0]).powi(2) + (p[1] - q[1]).powi(2)).sqrt();
    let (bc, ca, ab) = (length(b, c), length(c, a), length(a, b));
    let perimeter = bc + ca + ab;
    let area = ((b[0] - a[0]) * (c[1] - a[1]) - (c[0] - a[0]) * (b[1] - a[1])).abs() / 2.0;
    let inradius = 2.0 * area / perimeter;
    let incentre = [
        (bc * a[0] + ca * b[0] + ab * c[0]) / perimeter,
        (bc * a[1] + ca * b[1] + ab * c[1]) / perimeter,
    ];
    let share = ((inradius - distance) / inradius).max(0.0);
    outer.map(|[x, y]| {
        [
            incentre[0] + (x - incentre[0]) * share,
            incentre[1] + (y - incentre[1]) * share,
        ]
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bounds_stop_at_the_screen_edge() {
        let arrow = Arrow::new(800, 600);
        assert_eq!(
            arrow.bounds((799, 599)),
            Rect {
                x: 799,
                y: 599,
                width: 1,
                height: 1
            }
        );
        assert_eq!(
            arrow.bounds((0, 0)),
            Rect {
                x: 0,
                y: 0,
                width: 14,
                height: 20
            }
        );
        assert_eq!(arrow.bounds((800, 600)).width, 0, "off screen is empty");
    }

    #[test]
    fn buttons_pick_the_paints() {
        let idle = Buttons::default();
        assert_eq!(
            (Fill::of(idle), Outline::of(idle)),
            (Fill::Idle, Outline::Idle)
        );
        let both = Buttons {
            left: true,
            right: true,
            middle: true,
        };
        assert_eq!(Fill::of(both), Fill::Primary, "left wins over right");
        assert_eq!(Outline::of(both), Outline::Middle);
        assert_eq!(
            Fill::of(Buttons {
                right: true,
                ..idle
            }),
            Fill::Secondary
        );
    }

    #[test]
    fn inset_keeps_the_edges_at_the_distance() {
        // This right isosceles triangle has legs of 10. Its inradius is (10 + 10 - 10√2) / 2.
        let outer = [[0.0, 0.0], [0.0, 10.0], [10.0, 0.0]];
        let inner = inset(outer, 1.0);
        let [a, b, c] = inner;
        assert!((a[0] - 1.0).abs() < 1e-5 && (a[1] - 1.0).abs() < 1e-5);
        assert!(
            (b[0] - 1.0).abs() < 1e-5,
            "left edge stays vertical at x = 1"
        );
        assert!(
            (c[1] - 1.0).abs() < 1e-5,
            "top edge stays horizontal at y = 1"
        );
        let hypotenuse = |p: [f32; 2]| (p[0] + p[1] - 10.0) / 2f32.sqrt();
        assert!((hypotenuse(b) + 1.0).abs() < 1e-5);
        assert!((hypotenuse(c) + 1.0).abs() < 1e-5);
    }

    #[test]
    fn vertices_map_the_hotspot_to_clip_space() {
        let arrow = Arrow::new(800, 600);
        let vertices = arrow.vertices((400, 300), Buttons::default());
        assert_eq!(vertices[0].clip, [0.0, 0.0], "apex at the centre");
        assert_eq!(vertices[0].rgb, Outline::Idle.rgb());
        assert_eq!(vertices[3].rgb, Fill::Idle.rgb());
        assert!(
            vertices[3].clip[1] < 0.0,
            "inner apex is below the hotspot on screen"
        );
        assert!(vertices[1].clip[1] < vertices[0].clip[1], "y points up");
    }

    #[test]
    fn rgb_matches_within_tolerance() {
        let white = Fill::Idle.rgb();
        assert!(white.matches(0xFFFF_FFFF));
        assert!(white.matches(0x00F8_F8F8));
        assert!(!white.matches(0xFFE0_FFFF));
        let primary = Fill::Primary.rgb();
        assert!(primary.matches(0xFF33_80F2));
        assert!(!primary.matches(0xFF33_CC66));
    }
}
