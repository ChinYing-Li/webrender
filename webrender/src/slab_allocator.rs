/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

use euclid::{point2, size2, default::Box2D};
use api::units::{DeviceIntPoint, DeviceIntRect, DeviceIntSize};
use crate::internal_types::CacheTextureId;
use std::cmp;

#[derive(Copy, Clone, Debug)]
#[cfg_attr(feature = "capture", derive(Serialize))]
#[cfg_attr(feature = "replay", derive(Deserialize))]
pub enum SlabSizes {
    Default,
    Glyphs,
}

impl SlabSizes {
    fn get(&self, requested_size: DeviceIntSize) -> SlabSize {
        match *self {
            SlabSizes::Default => Self::default_slab_size(requested_size),
            SlabSizes::Glyphs => Self::glyphs_slab_size(requested_size),
        }
    }

    fn default_slab_size(size: DeviceIntSize) -> SlabSize {
        fn quantize_dimension(size: i32) -> i32 {
            match size {
                0 => unreachable!(),
                1..=16 => 16,
                17..=32 => 32,
                33..=64 => 64,
                65..=128 => 128,
                129..=256 => 256,
                257..=512 => 512,
                _ => panic!("Invalid dimensions for cache!"),
            }
        }


        let x_size = quantize_dimension(size.width);
        let y_size = quantize_dimension(size.height);

        let (width, height) = match (x_size, y_size) {
            // Special cased rectangular slab pages.
            (512, 0..=64) => (512, 64),
            (512, 128) => (512, 128),
            (512, 256) => (512, 256),
            (0..=64, 512) => (64, 512),
            (128, 512) => (128, 512),
            (256, 512) => (256, 512),

            // If none of those fit, use a square slab size.
            (x_size, y_size) => {
                let square_size = cmp::max(x_size, y_size);
                (square_size, square_size)
            }
        };

        SlabSize {
            width,
            height,
        }
    }

    fn glyphs_slab_size(size: DeviceIntSize) -> SlabSize {
        fn quantize_dimension(size: i32) -> i32 {
            match size {
                0 => unreachable!(),
                1..=8 => 8,
                9..=16 => 16,
                17..=32 => 32,
                33..=64 => 64,
                65..=128 => 128,
                _ => panic!("Invalid dimensions for cache!"),
            }
        }


        let x_size = quantize_dimension(size.width);
        let y_size = quantize_dimension(size.height);

        let (width, height) = match (x_size, y_size) {
            // Special cased rectangular slab pages.
            (8, 16) => (8, 16),
            (16, 32) => (16, 32),

            // If none of those fit, use a square slab size.
            (x_size, y_size) => {
                let square_size = cmp::max(x_size, y_size);
                (square_size, square_size)
            }
        };

        SlabSize {
            width,
            height,
        }
    }
}

#[cfg_attr(feature = "capture", derive(Serialize))]
#[cfg_attr(feature = "replay", derive(Deserialize))]
#[derive(Copy, Clone, PartialEq)]
struct SlabSize {
    width: i32,
    height: i32,
}

impl SlabSize {
    fn invalid() -> SlabSize {
        SlabSize {
            width: 0,
            height: 0,
        }
    }
}

// The x/y location within a texture region of an allocation.
#[cfg_attr(feature = "capture", derive(Serialize))]
#[cfg_attr(feature = "replay", derive(Deserialize))]
struct TextureLocation(pub u8, pub u8);

impl TextureLocation {
    fn new(x: i32, y: i32) -> Self {
        debug_assert!(x >= 0 && y >= 0 && x < 0x100 && y < 0x100);
        TextureLocation(x as u8, y as u8)
    }
}

/// A region is a rectangular part of a texture cache texture, split into fixed-size slabs.
#[cfg_attr(feature = "capture", derive(Serialize))]
#[cfg_attr(feature = "replay", derive(Deserialize))]
struct TextureRegion {
    index: usize,
    slab_size: SlabSize,
    offset: DeviceIntPoint,
    free_slots: Vec<TextureLocation>,
    total_slot_count: usize,
}

impl TextureRegion {
    fn new(index: usize, offset: DeviceIntPoint) -> Self {
        TextureRegion {
            index,
            slab_size: SlabSize::invalid(),
            offset,
            free_slots: Vec::new(),
            total_slot_count: 0,
        }
    }

    // Initialize a region to be an allocator for a specific slab size.
    fn init(&mut self, slab_size: SlabSize, region_size: i32, empty_regions: &mut usize) {
        debug_assert!(self.slab_size == SlabSize::invalid());
        debug_assert!(self.free_slots.is_empty());

        self.slab_size = slab_size;
        let slots_per_x_axis = region_size / self.slab_size.width;
        let slots_per_y_axis = region_size / self.slab_size.height;

        // Add each block to a freelist.
        for y in 0 .. slots_per_y_axis {
            for x in 0 .. slots_per_x_axis {
                self.free_slots.push(TextureLocation::new(x, y));
            }
        }

        self.total_slot_count = self.free_slots.len();
        *empty_regions -= 1;
    }

    // Deinit a region, allowing it to become a region with
    // a different allocator size.
    fn deinit(&mut self, empty_regions: &mut usize) {
        self.slab_size = SlabSize::invalid();
        self.free_slots.clear();
        self.total_slot_count = 0;
        *empty_regions += 1;
    }

    fn is_empty(&self) -> bool {
        self.slab_size == SlabSize::invalid()
    }

    // Attempt to allocate a fixed size block from this region.
    fn alloc(&mut self) -> Option<DeviceIntPoint> {
        debug_assert!(self.slab_size != SlabSize::invalid());

        self.free_slots.pop().map(|location| {
            point2(
                self.offset.x + self.slab_size.width * location.0 as i32,
                self.offset.y + self.slab_size.height * location.1 as i32,
            )
        })
    }

    // Free a block in this region.
    fn free(&mut self, point: DeviceIntPoint, empty_regions: &mut usize) {
        let x = (point.x - self.offset.x) / self.slab_size.width;
        let y = (point.y - self.offset.y) / self.slab_size.height;
        self.free_slots.push(TextureLocation::new(x, y));

        // If this region is completely unused, deinit it
        // so that it can become a different slab size
        // as required.
        if self.free_slots.len() == self.total_slot_count {
            self.deinit(empty_regions);
        }
    }
}

/// A 2D texture divided into regions.
#[cfg_attr(feature = "capture", derive(Serialize))]
#[cfg_attr(feature = "replay", derive(Deserialize))]
pub struct TextureUnit {
    texture_id: CacheTextureId,
    regions: Vec<TextureRegion>,
    size: i32,
    region_size: i32,
    empty_regions: usize,
    slab_sizes: SlabSizes,
}

impl TextureUnit {
    pub fn new(texture_id: CacheTextureId, size: i32, region_size: i32, slab_sizes: SlabSizes) -> Self {
        let regions_per_row = size / region_size;
        let num_regions = (regions_per_row * regions_per_row) as usize;

        let mut regions = Vec::with_capacity(num_regions);

        for index in 0..num_regions {
            let offset = point2(
                (index as i32 % regions_per_row) * region_size,
                (index as i32 / regions_per_row) * region_size,
            );

            regions.push(TextureRegion::new(index, offset));
        }

        TextureUnit {
            texture_id,
            regions,
            region_size,
            size,
            empty_regions: num_regions,
            slab_sizes,
        }
    }

    pub fn texture_id(&self) -> CacheTextureId {
        self.texture_id
    }

    pub fn is_empty(&self) -> bool {
        self.empty_regions == self.regions.len()
    }

    // Returns the region index and allocated rect.
    pub fn allocate(&mut self, size: DeviceIntSize) -> Option<(usize, DeviceIntRect)> {
        let slab_size = self.slab_sizes.get(size);

        // Keep track of the location of an empty region,
        // in case we need to select a new empty region
        // after the loop.
        let mut empty_region_index = None;

        let allocated_size = size2(slab_size.width, slab_size.height);

        // Run through the existing regions of this size, and see if
        // we can find a free block in any of them.
        for (i, region) in self.regions.iter_mut().enumerate() {
            if region.is_empty() {
                empty_region_index = Some(i);
            } else if region.slab_size == slab_size {
                if let Some(location) = region.alloc() {
                    return Some((
                        region.index,
                        DeviceIntRect {
                            origin: location,
                            size: allocated_size,
                        }
                    ));
                }
            }
        }

        if let Some(empty_region_index) = empty_region_index {
            let region = &mut self.regions[empty_region_index];
            region.init(slab_size, self.region_size, &mut self.empty_regions);

            return Some((
                region.index,
                DeviceIntRect {
                    origin: region.alloc().unwrap(),
                    size: allocated_size,
                },
            ))
        }

        None
    }

    pub fn deallocate(&mut self, origin: DeviceIntPoint, region_index: usize) -> DeviceIntSize {
        let region = &mut self.regions[region_index];
        region.free(origin, &mut self.empty_regions);

        size2(region.slab_size.width, region.slab_size.height)
    }

    pub fn num_regions(&self) -> usize {
        self.regions.len()
    }

    pub fn dump_as_svg(&self, rect: &Box2D<f32>, output: &mut dyn std::io::Write) -> std::io::Result<()> {
        use svg_fmt::*;

        let region_spacing = 5.0;
        let text_spacing = 15.0;
        let regions_per_row = (self.size / self.region_size) as usize;
        let wh = rect.size().width.min(rect.size().height);
        let region_wh = (wh - region_spacing) / regions_per_row as f32 - region_spacing;

        let x0 = rect.min.x;
        let y0 = rect.min.y;

        for (idx, region) in self.regions.iter().enumerate() {
            let slab_size = region.slab_size;
            let x = x0 + (idx % regions_per_row) as f32 * (region_wh + region_spacing);

            let y = y0 + text_spacing + (idx / regions_per_row) as f32 * (region_wh + region_spacing);

            let texture_background = if region.is_empty() { rgb(30, 30, 30) } else { rgb(40, 40, 130) };
            writeln!(output, "    {}", rectangle(x, y, region_wh, region_wh).inflate(1.0, 1.0).fill(rgb(10, 10, 10)))?;
            writeln!(output, "    {}", rectangle(x, y, region_wh, region_wh).fill(texture_background))?;

            let sw = (slab_size.width as f32 / self.region_size as f32) * region_wh;
            let sh = (slab_size.height as f32 / self.region_size as f32) * region_wh;

            for slot in &region.free_slots {
                let sx = x + slot.0 as f32 * sw;
                let sy = y + slot.1 as f32 * sh;

                // Allocation slot.
                writeln!(output, "    {}", rectangle(sx, sy, sw, sh).inflate(-0.5, -0.5).fill(rgb(30, 30, 30)))?;
            }

            if slab_size.width != 0 {
                let region_text = format!("{}x{}", slab_size.width, slab_size.height);
                let tx = x + 1.0;
                let ty = y + region_wh - 1.0;
                writeln!(output, "    {}", text(tx, ty, region_text).color(rgb(230, 230, 230)))?;
            }
        }

        Ok(())
    }
}
