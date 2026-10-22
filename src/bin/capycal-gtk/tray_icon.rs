//! Tray icon images: the app icon with a badge for the state, as the ARGB
//! pixmaps the StatusNotifierItem protocol takes.

use crate::menu::Icon;

/// An RGBA image, 4 bytes per pixel, rows without padding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rgba {
    pub size: usize,
    pub pixels: Vec<u8>,
}

const RED: [u8; 3] = [0xE0, 0x1B, 0x24];
const BLUE: [u8; 3] = [0x35, 0x84, 0xE4];

impl Rgba {
    /// The icon for `state`: a red badge for errors, a blue one while
    /// syncing, faded when inactive.
    pub fn for_state(&self, state: Icon) -> Rgba {
        let mut icon = self.clone();
        match state {
            Icon::Error => icon.badge(RED),
            Icon::Syncing => icon.badge(BLUE),
            Icon::Inactive => icon.fade(),
            Icon::Ok => {}
        }
        icon
    }

    /// A filled dot with a white ring in the bottom right corner.
    fn badge(&mut self, color: [u8; 3]) {
        let size = self.size as f64;
        let radius = size * 0.22;
        let ring = size * 0.06;
        let center = size - radius - ring;
        for y in 0..self.size {
            for x in 0..self.size {
                let dx = x as f64 + 0.5 - center;
                let dy = y as f64 + 0.5 - center;
                let distance = (dx * dx + dy * dy).sqrt();
                let rgb = if distance <= radius {
                    color
                } else if distance <= radius + ring {
                    [0xFF; 3]
                } else {
                    continue;
                };
                let i = (y * self.size + x) * 4;
                self.pixels[i..i + 4].copy_from_slice(&[rgb[0], rgb[1], rgb[2], 0xFF]);
            }
        }
    }

    /// Gray and half transparent.
    fn fade(&mut self) {
        for pixel in self.pixels.as_chunks_mut::<4>().0 {
            let gray =
                (0.3 * pixel[0] as f64 + 0.59 * pixel[1] as f64 + 0.11 * pixel[2] as f64) as u8;
            pixel[..3].fill(gray);
            pixel[3] /= 2;
        }
    }

    /// The pixels as ARGB32 in network byte order.
    pub fn argb(&self) -> Vec<u8> {
        self.pixels
            .as_chunks::<4>()
            .0
            .iter()
            .flat_map(|p| [p[3], p[0], p[1], p[2]])
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn white(size: usize) -> Rgba {
        Rgba {
            size,
            pixels: vec![0xFF; size * size * 4],
        }
    }

    fn pixel(icon: &Rgba, x: usize, y: usize) -> [u8; 4] {
        let i = (y * icon.size + x) * 4;
        icon.pixels[i..i + 4].try_into().unwrap()
    }

    #[test]
    fn test_badges_mark_the_bottom_right_corner() {
        let base = white(32);
        let error = base.for_state(Icon::Error);
        assert_eq!(pixel(&error, 24, 24), [0xE0, 0x1B, 0x24, 0xFF]);
        assert_eq!(pixel(&error, 4, 4), [0xFF; 4]);
        let syncing = base.for_state(Icon::Syncing);
        assert_eq!(pixel(&syncing, 24, 24), [0x35, 0x84, 0xE4, 0xFF]);
        assert_eq!(base.for_state(Icon::Ok), base);
    }

    #[test]
    fn test_inactive_is_gray_and_faded() {
        let mut base = white(2);
        base.pixels[..4].copy_from_slice(&[0xFF, 0, 0, 0xFF]);
        let faded = base.for_state(Icon::Inactive);
        assert_eq!(pixel(&faded, 0, 0), [76, 76, 76, 0x7F]);
    }

    #[test]
    fn test_argb_order() {
        let icon = Rgba {
            size: 1,
            pixels: vec![1, 2, 3, 4],
        };
        assert_eq!(icon.argb(), [4, 1, 2, 3]);
    }
}
