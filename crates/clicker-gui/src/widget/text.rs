//! DirectWrite text for widget labels and the CPS readout.
//!
//! Greyscale antialiasing, not ClearType: subpixel AA against the transparent
//! DirectComposition surface produces colour fringing. Text formats are cached
//! per style; creating them per frame is wasteful.

use crate::render::color::Rgb;
use crate::render::device::RenderError;
use windows::core::PCWSTR;
use windows::Win32::Graphics::Direct2D::Common::{D2D1_COLOR_F, D2D_RECT_F};
use windows::Win32::Graphics::Direct2D::{
    ID2D1DeviceContext, D2D1_DRAW_TEXT_OPTIONS_NONE, D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE,
};
use windows::Win32::Graphics::DirectWrite::{
    DWriteCreateFactory, IDWriteFactory, IDWriteTextFormat, DWRITE_FACTORY_TYPE_SHARED,
    DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_WEIGHT,
    DWRITE_FONT_WEIGHT_NORMAL, DWRITE_FONT_WEIGHT_SEMI_BOLD, DWRITE_MEASURING_MODE_NATURAL,
    DWRITE_PARAGRAPH_ALIGNMENT_CENTER, DWRITE_TEXT_ALIGNMENT_CENTER,
};

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum TextStyle {
    Body,
    Large,
}

pub struct TextRenderer {
    _factory: IDWriteFactory,
    body: IDWriteTextFormat,
    large: IDWriteTextFormat,
}

fn make_format(
    f: &IDWriteFactory,
    size: f32,
    weight: DWRITE_FONT_WEIGHT,
) -> Result<IDWriteTextFormat, RenderError> {
    // Segoe UI Variable if present; DirectWrite falls back to Segoe UI otherwise.
    let family: Vec<u16> = "Segoe UI Variable\0".encode_utf16().collect();
    let locale: Vec<u16> = "en-us\0".encode_utf16().collect();
    // SAFETY: both string pointers are NUL-terminated UTF-16 that outlive the
    // call; all enum arguments are valid values.
    let fmt = unsafe {
        f.CreateTextFormat(
            PCWSTR(family.as_ptr()),
            None,
            weight,
            DWRITE_FONT_STYLE_NORMAL,
            DWRITE_FONT_STRETCH_NORMAL,
            size,
            PCWSTR(locale.as_ptr()),
        )?
    };
    // SAFETY: `fmt` is a live text format; both setters take plain enums.
    unsafe {
        let _ = fmt.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_CENTER);
        let _ = fmt.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER);
    }
    Ok(fmt)
}

impl TextRenderer {
    pub fn new() -> Result<Self, RenderError> {
        // SAFETY: creates a shared DWrite factory; T is fixed by the annotation.
        let factory: IDWriteFactory = unsafe { DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)? };
        let body = make_format(&factory, 15.0, DWRITE_FONT_WEIGHT_NORMAL)?;
        let large = make_format(&factory, 44.0, DWRITE_FONT_WEIGHT_SEMI_BOLD)?;
        Ok(Self { _factory: factory, body, large })
    }

    pub fn format(&self, style: TextStyle) -> &IDWriteTextFormat {
        match style {
            TextStyle::Body => &self.body,
            TextStyle::Large => &self.large,
        }
    }
}

/// Draw greyscale-AA text centred in `rect`.
///
/// # Safety
/// `ctx` must be a live device context inside a `BeginDraw`.
pub unsafe fn draw_text(
    ctx: &ID2D1DeviceContext,
    text: &str,
    rect: D2D_RECT_F,
    format: &IDWriteTextFormat,
    color: Rgb,
) -> Result<(), RenderError> {
    let utf16: Vec<u16> = text.encode_utf16().collect();
    // SAFETY: ctx is live and mid-draw; the brush and utf16 buffer outlive the
    // call. `DrawText` here resolves to the inherited render-target method (no
    // SVG glyph style / palette index).
    unsafe {
        ctx.SetTextAntialiasMode(D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE);
        let brush = ctx.CreateSolidColorBrush(
            &D2D1_COLOR_F { r: color.r, g: color.g, b: color.b, a: 1.0 },
            None,
        )?;
        ctx.DrawText(
            &utf16,
            format,
            &rect,
            &brush,
            D2D1_DRAW_TEXT_OPTIONS_NONE,
            DWRITE_MEASURING_MODE_NATURAL,
        );
    }
    Ok(())
}
