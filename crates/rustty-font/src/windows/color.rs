//! Color glyphs use Windows' native COLR/SVG/bitmap rendering on a software
//! Direct2D device. The resulting pixels enter the same atlas as outline glyphs.

use super::{BitmapFormat, GlyphBitmap, Result, error};
use ::windows::{
    Win32::{
        Foundation::HMODULE,
        Graphics::{
            Direct2D::{Common::*, *},
            Direct3D::D3D_DRIVER_TYPE_WARP,
            Direct3D11::*,
            DirectWrite::*,
            Dxgi::{Common::DXGI_FORMAT_B8G8R8A8_UNORM, IDXGIDevice},
        },
    },
    core::Interface,
};
use windows_numerics::Vector2;

pub(super) struct Renderer {
    context: ID2D1DeviceContext4,
}

impl Renderer {
    pub(super) fn new() -> Result<Self> {
        let mut device = None;
        // WARP is software rendering: font rasterization must not depend on a
        // display adapter, or compete with the application's WGPU device.
        unsafe {
            D3D11CreateDevice(
                None,
                D3D_DRIVER_TYPE_WARP,
                HMODULE::default(),
                D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                None,
                D3D11_SDK_VERSION,
                Some(&mut device),
                None,
                None,
            )
        }
        .map_err(error)?;
        let dxgi: IDXGIDevice = device.unwrap().cast().map_err(error)?;
        let device = unsafe { D2D1CreateDevice(&dxgi, None) }.map_err(error)?;
        let context = unsafe { device.CreateDeviceContext(D2D1_DEVICE_CONTEXT_OPTIONS_NONE) }
            .map_err(error)?
            .cast()
            .map_err(error)?;
        Ok(Self { context })
    }

    pub(super) fn rasterize(
        &self,
        factory: &IDWriteFactory7,
        run: &DWRITE_GLYPH_RUN,
        formats: DWRITE_GLYPH_IMAGE_FORMATS,
    ) -> Result<GlyphBitmap> {
        let margin = (run.fontEmSize * 2.0).ceil() as u32 + 2;
        let side = margin
            .checked_mul(2)
            .filter(|n| *n <= 8192)
            .ok_or_else(|| error("color glyph exceeds rasterization limits"))?;
        let origin = Vector2 {
            X: margin as f32,
            Y: margin as f32,
        };
        let properties = D2D1_BITMAP_PROPERTIES1 {
            pixelFormat: D2D1_PIXEL_FORMAT {
                format: DXGI_FORMAT_B8G8R8A8_UNORM,
                alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
            },
            dpiX: 96.0,
            dpiY: 96.0,
            bitmapOptions: D2D1_BITMAP_OPTIONS_TARGET,
            ..Default::default()
        };
        let size = D2D_SIZE_U {
            width: side,
            height: side,
        };
        let target =
            unsafe { self.context.CreateBitmap(size, None, 0, &properties) }.map_err(error)?;
        let read_properties = D2D1_BITMAP_PROPERTIES1 {
            bitmapOptions: D2D1_BITMAP_OPTIONS_CPU_READ | D2D1_BITMAP_OPTIONS_CANNOT_DRAW,
            ..properties
        };
        let readback =
            unsafe { self.context.CreateBitmap(size, None, 0, &read_properties) }.map_err(error)?;
        let brush = unsafe {
            self.context.CreateSolidColorBrush(
                &D2D1_COLOR_F {
                    r: 1.0,
                    g: 1.0,
                    b: 1.0,
                    a: 1.0,
                },
                None,
            )
        }
        .map_err(error)?;
        unsafe {
            self.context.SetTarget(&target);
            self.context
                .SetTextAntialiasMode(D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE);
            self.context.BeginDraw();
            self.context.Clear(None);
        }
        // Finish and release the target even if a native enumeration fails.
        let drawing = self.draw(factory, run, formats, origin, &brush);
        let finished = unsafe { self.context.EndDraw(None, None) };
        unsafe {
            self.context.SetTarget(None);
        }
        drawing?;
        finished.map_err(error)?;
        unsafe { readback.CopyFromBitmap(None, &target, None) }.map_err(error)?;
        let mapped = unsafe { readback.Map(D2D1_MAP_OPTIONS_READ) }.map_err(error)?;
        // SAFETY: Map owns this pitch*height memory until the matching Unmap.
        let source = unsafe {
            std::slice::from_raw_parts(mapped.bits, mapped.pitch as usize * side as usize)
        };
        let (mut left, mut top, mut right, mut bottom) = (side, side, 0, 0);
        for y in 0..side {
            for x in 0..side {
                if source[(y * mapped.pitch + x * 4 + 3) as usize] != 0 {
                    left = left.min(x);
                    top = top.min(y);
                    right = right.max(x + 1);
                    bottom = bottom.max(y + 1);
                }
            }
        }
        let mut pixels = Vec::new();
        if right > left && bottom > top {
            pixels.reserve(((right - left) * (bottom - top) * 4) as usize);
            for y in top..bottom {
                for x in left..right {
                    let i = (y * mapped.pitch + x * 4) as usize;
                    pixels.extend_from_slice(&[
                        source[i + 2],
                        source[i + 1],
                        source[i],
                        source[i + 3],
                    ]);
                }
            }
        } else {
            left = 0;
            top = 0;
            right = 0;
            bottom = 0;
        }
        unsafe { readback.Unmap() }.map_err(error)?;
        if !pixels.is_empty() && (left == 0 || right == side || top == 0 || bottom == side) {
            return Err(error("color glyph exceeds rasterization bounds"));
        }
        Ok(GlyphBitmap {
            width: right - left,
            height: bottom - top,
            bearing_x: left as i32 - margin as i32,
            bearing_y: margin as i32 - top as i32,
            format: BitmapFormat::Rgba,
            pixels,
        })
    }

    fn draw(
        &self,
        factory: &IDWriteFactory7,
        run: &DWRITE_GLYPH_RUN,
        formats: DWRITE_GLYPH_IMAGE_FORMATS,
        origin: Vector2,
        brush: &ID2D1SolidColorBrush,
    ) -> Result<()> {
        if (formats & DWRITE_GLYPH_IMAGE_FORMATS_COLR_PAINT_TREE).0 != 0
            && let Ok(context) = self.context.cast::<ID2D1DeviceContext7>()
        {
            unsafe {
                context.DrawPaintGlyphRun(origin, run, brush, 0, DWRITE_MEASURING_MODE_NATURAL);
            }
            return Ok(());
        }
        let desired = DWRITE_GLYPH_IMAGE_FORMATS_TRUETYPE
            | DWRITE_GLYPH_IMAGE_FORMATS_CFF
            | DWRITE_GLYPH_IMAGE_FORMATS_COLR
            | DWRITE_GLYPH_IMAGE_FORMATS_SVG
            | DWRITE_GLYPH_IMAGE_FORMATS_PNG
            | DWRITE_GLYPH_IMAGE_FORMATS_JPEG
            | DWRITE_GLYPH_IMAGE_FORMATS_TIFF
            | DWRITE_GLYPH_IMAGE_FORMATS_PREMULTIPLIED_B8G8R8A8;
        let enumeration = unsafe {
            factory.TranslateColorGlyphRun(
                origin,
                run,
                None,
                desired,
                DWRITE_MEASURING_MODE_NATURAL,
                None,
                0,
            )
        }
        .map_err(error)?;
        while unsafe { enumeration.MoveNext() }.map_err(error)?.as_bool() {
            let layer = unsafe { enumeration.GetCurrentRun() }.map_err(error)?;
            let layer = unsafe { &*layer };
            let origin = Vector2 {
                X: layer.Base.baselineOriginX,
                Y: layer.Base.baselineOriginY,
            };
            let run = &layer.Base.glyphRun;
            let format = layer.glyphImageFormat;
            if format == DWRITE_GLYPH_IMAGE_FORMATS_SVG {
                unsafe {
                    self.context.DrawSvgGlyphRun(
                        origin,
                        run,
                        brush,
                        None,
                        0,
                        DWRITE_MEASURING_MODE_NATURAL,
                    );
                }
            } else if (format
                & (DWRITE_GLYPH_IMAGE_FORMATS_PNG
                    | DWRITE_GLYPH_IMAGE_FORMATS_JPEG
                    | DWRITE_GLYPH_IMAGE_FORMATS_TIFF
                    | DWRITE_GLYPH_IMAGE_FORMATS_PREMULTIPLIED_B8G8R8A8))
                .0
                != 0
            {
                unsafe {
                    self.context.DrawColorBitmapGlyphRun(
                        format,
                        origin,
                        run,
                        DWRITE_MEASURING_MODE_NATURAL,
                        D2D1_COLOR_BITMAP_GLYPH_SNAP_OPTION_DISABLE,
                    );
                }
            } else {
                let color = layer.Base.runColor;
                unsafe {
                    brush.SetColor(&D2D1_COLOR_F {
                        r: color.r,
                        g: color.g,
                        b: color.b,
                        a: color.a,
                    });
                    self.context.DrawGlyphRun(
                        origin,
                        run,
                        None,
                        brush,
                        DWRITE_MEASURING_MODE_NATURAL,
                    );
                }
            }
        }
        Ok(())
    }
}
