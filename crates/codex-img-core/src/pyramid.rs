//! Tile pyramids for big tiled maps: the full map at 1/2, 1/4, … of its size, each level cut into
//! tiles on the same x_y grid (x the column, y the row), for zoomed-out views.
use crate::transform;
use image::{imageops, RgbaImage};

/// Tile `(x, y)` covers this rectangle of a `width` x `height` level; edge tiles are cropped.
pub fn tile_rect(width: u32, height: u32, tile: u32, x: u32, y: u32) -> (u32, u32, u32, u32) {
    let (left, top) = (x * tile, y * tile);
    (left, top, tile.min(width - left), tile.min(height - top))
}

/// Size of level `level` of a `width` x `height` map: halved per level, rounded up.
pub fn level_size(width: u32, height: u32, level: u32) -> (u32, u32) {
    let scale = 1u64 << level;
    let shrink = |n: u32| (u64::from(n).div_ceil(scale)).max(1) as u32;
    (shrink(width), shrink(height))
}

/// How many levels there are, level 0 included: until a level fits in one tile, at most `limit`.
pub fn level_count(width: u32, height: u32, tile: u32, limit: Option<u32>) -> u32 {
    let mut count = 1;
    while limit.is_none_or(|l| count < l) {
        let (w, h) = level_size(width, height, count - 1);
        if w <= tile && h <= tile {
            break;
        }
        count += 1;
    }
    count
}

/// Level `level` of `map`, resampled from the full map with premultiplied alpha, so no rounding
/// compounds from level to level.
pub fn level(map: &RgbaImage, level: u32) -> RgbaImage {
    let (w, h) = level_size(map.width(), map.height(), level);
    transform::resample(map, w, h)
}

/// The tiles of `image` on a `tile` grid, row by row. Fully transparent tiles are None.
pub fn cut(image: &RgbaImage, tile: u32) -> Vec<(u32, u32, Option<RgbaImage>)> {
    let (w, h) = image.dimensions();
    let (cols, rows) = (w.div_ceil(tile), h.div_ceil(tile));
    let mut tiles = Vec::with_capacity((cols * rows) as usize);
    for y in 0..rows {
        for x in 0..cols {
            let (left, top, tw, th) = tile_rect(w, h, tile, x, y);
            let piece = imageops::crop_imm(image, left, top, tw, th).to_image();
            let blank = piece.pixels().all(|p| p.0[3] == 0);
            tiles.push((x, y, (!blank).then_some(piece)));
        }
    }
    tiles
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::Rgba;

    #[test]
    fn sizes_and_counts_levels() {
        assert_eq!(level_size(3500, 2000, 0), (3500, 2000));
        assert_eq!(level_size(3500, 2000, 1), (1750, 1000));
        assert_eq!(level_size(3500, 2000, 3), (438, 250));
        // 3500 → 1750 → 875 → 438 → 219: five levels until one 256 tile holds it.
        assert_eq!(level_count(3500, 2000, 256, None), 5);
        assert_eq!(level_count(3500, 2000, 1024, None), 3);
        assert_eq!(level_count(3500, 2000, 256, Some(2)), 2);
        assert_eq!(level_count(100, 100, 256, None), 1);
        assert_eq!(tile_rect(3500, 2000, 256, 13, 7), (3328, 1792, 172, 208));
    }

    #[test]
    fn cuts_on_the_grid_and_skips_blank_tiles() {
        let mut image = RgbaImage::new(5, 3);
        image.put_pixel(4, 2, Rgba([1, 2, 3, 255]));
        let tiles = cut(&image, 2);
        let coords: Vec<(u32, u32)> = tiles.iter().map(|(x, y, _)| (*x, *y)).collect();
        assert_eq!(coords, [(0, 0), (1, 0), (2, 0), (0, 1), (1, 1), (2, 1)]);
        assert_eq!(tiles.iter().filter(|t| t.2.is_some()).count(), 1);
        let corner = tiles[5].2.as_ref().unwrap();
        assert_eq!(corner.dimensions(), (1, 1));
        assert_eq!(corner.get_pixel(0, 0).0, [1, 2, 3, 255]);
        assert_eq!(level(&image, 1).dimensions(), (3, 2));
    }
}
