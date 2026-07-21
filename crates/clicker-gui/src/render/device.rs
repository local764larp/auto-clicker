//! The rendering device stack.
//!
//! `ID3D11Device` → `IDXGIDevice` → `ID2D1Device` → `ID2D1DeviceContext`,
//! presented through a DirectComposition visual tree with a flip-model
//! swapchain.
//!
//! DirectComposition is required rather than decorative: a borderless window
//! with rounded corners needs true per-pixel transparency, which a plain HWND
//! swapchain cannot give. `DXGI_ALPHA_MODE_PREMULTIPLIED` on a composition
//! swapchain is what makes the corners actually transparent instead of black.

use windows::core::{Interface, HRESULT};
use windows::Win32::Foundation::{E_FAIL, HMODULE, HWND};
use windows::Win32::Graphics::Direct2D::Common::{
    D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_PIXEL_FORMAT,
};
use windows::Win32::Graphics::Direct2D::{
    D2D1CreateFactory, ID2D1Bitmap1, ID2D1Device, ID2D1DeviceContext, ID2D1Factory1,
    D2D1_BITMAP_OPTIONS_CANNOT_DRAW, D2D1_BITMAP_OPTIONS_TARGET, D2D1_BITMAP_PROPERTIES1,
    D2D1_DEVICE_CONTEXT_OPTIONS_NONE, D2D1_FACTORY_TYPE_SINGLE_THREADED,
};
use windows::Win32::Graphics::Direct3D::{D3D_DRIVER_TYPE_HARDWARE, D3D_DRIVER_TYPE_WARP};
use windows::Win32::Graphics::Direct3D11::{
    D3D11CreateDevice, ID3D11Device, D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_SDK_VERSION,
};
use windows::Win32::Graphics::DirectComposition::{
    DCompositionCreateDevice, IDCompositionDevice, IDCompositionTarget, IDCompositionVisual,
};
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_ALPHA_MODE_PREMULTIPLIED, DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC,
};
use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory2, IDXGIDevice, IDXGIFactory2, IDXGISurface, IDXGISwapChain1,
    DXGI_CREATE_FACTORY_FLAGS, DXGI_ERROR_DEVICE_HUNG, DXGI_ERROR_DEVICE_REMOVED,
    DXGI_ERROR_DEVICE_RESET, DXGI_ERROR_DRIVER_INTERNAL_ERROR, DXGI_SCALING_STRETCH,
    DXGI_SWAP_CHAIN_DESC1, DXGI_SWAP_CHAIN_FLAG, DXGI_SWAP_EFFECT_FLIP_DISCARD,
    DXGI_USAGE_RENDER_TARGET_OUTPUT,
};

/// `D2DERR_RECREATE_TARGET`. Not re-exported by the `windows` crate at this
/// version, so it is spelled out; the value is stable ABI.
const D2DERR_RECREATE_TARGET: HRESULT = HRESULT(0x8899_000Cu32 as i32);

/// Which D3D driver to create the stack on.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum DriverKind {
    /// The GPU. What ships.
    Hardware,
    /// Microsoft's software rasterizer. Bit-identical across machines, which is
    /// what makes golden-image comparison meaningful rather than flaky.
    Warp,
}

#[derive(Debug)]
pub struct RenderError(pub windows::core::Error);

impl core::fmt::Display for RenderError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "render device error: {}", self.0)
    }
}

impl std::error::Error for RenderError {}

impl From<windows::core::Error> for RenderError {
    fn from(e: windows::core::Error) -> Self {
        RenderError(e)
    }
}

/// True for errors that mean the device is gone and the stack must be rebuilt.
///
/// Driver updates and GPU resets trigger these in ordinary use. Ignoring them
/// leaves a permanently black window that users reasonably read as a hang.
pub fn is_device_lost(e: &windows::core::Error) -> bool {
    let hr = e.code();
    hr == DXGI_ERROR_DEVICE_REMOVED
        || hr == DXGI_ERROR_DEVICE_RESET
        || hr == DXGI_ERROR_DEVICE_HUNG
        || hr == DXGI_ERROR_DRIVER_INTERNAL_ERROR
        || hr == D2DERR_RECREATE_TARGET
}

pub struct RenderDevice {
    _d3d: ID3D11Device,
    _d2d_device: ID2D1Device,
    pub ctx: ID2D1DeviceContext,
    swapchain: IDXGISwapChain1,
    _dcomp_device: IDCompositionDevice,
    _dcomp_target: IDCompositionTarget,
    _dcomp_visual: IDCompositionVisual,
    target_bitmap: Option<ID2D1Bitmap1>,
    kind: DriverKind,
    size: (u32, u32),
    dpi_scale: f32,
}

impl RenderDevice {
    pub fn new(
        hwnd: HWND,
        kind: DriverKind,
        size: (u32, u32),
        dpi_scale: f32,
    ) -> Result<Self, RenderError> {
        let (w, h) = (size.0.max(1), size.1.max(1));

        let driver = match kind {
            DriverKind::Hardware => D3D_DRIVER_TYPE_HARDWARE,
            DriverKind::Warp => D3D_DRIVER_TYPE_WARP,
        };

        let mut d3d: Option<ID3D11Device> = None;
        // SAFETY: all out-params are valid Option slots the callee fills.
        // BGRA_SUPPORT is required for D2D interop; omitting it makes the
        // ID2D1Device creation below fail with a confusing error.
        unsafe {
            D3D11CreateDevice(
                None,
                driver,
                HMODULE::default(),
                D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                None,
                D3D11_SDK_VERSION,
                Some(&mut d3d),
                None,
                None,
            )?;
        }
        let d3d = d3d.ok_or_else(|| RenderError(windows::core::Error::from(E_FAIL)))?;

        let dxgi: IDXGIDevice = d3d.cast()?;

        // SAFETY: factory creation with no flags; the out-param is a valid slot.
        let factory: IDXGIFactory2 = unsafe { CreateDXGIFactory2(DXGI_CREATE_FACTORY_FLAGS(0))? };

        let desc = DXGI_SWAP_CHAIN_DESC1 {
            Width: w,
            Height: h,
            Format: DXGI_FORMAT_B8G8R8A8_UNORM,
            Stereo: false.into(),
            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
            BufferCount: 2,
            Scaling: DXGI_SCALING_STRETCH,
            SwapEffect: DXGI_SWAP_EFFECT_FLIP_DISCARD,
            // Premultiplied alpha is what makes rounded corners transparent
            // rather than black.
            AlphaMode: DXGI_ALPHA_MODE_PREMULTIPLIED,
            Flags: 0,
        };

        // SAFETY: `dxgi` is a live device; `desc` is fully initialised and read
        // only during the call. A composition swapchain takes no HWND — it is
        // bound to a visual instead, which is the whole point.
        let swapchain =
            unsafe { factory.CreateSwapChainForComposition(&dxgi, &desc, None)? };

        // SAFETY: creates the D2D factory; the type parameter selects the
        // returned interface and matches the annotation.
        let d2d_factory: ID2D1Factory1 =
            unsafe { D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)? };

        // SAFETY: `dxgi` is a live DXGI device owned by this stack.
        let d2d_device = unsafe { d2d_factory.CreateDevice(&dxgi)? };
        // SAFETY: single-threaded context on a device we own.
        let ctx = unsafe { d2d_device.CreateDeviceContext(D2D1_DEVICE_CONTEXT_OPTIONS_NONE)? };

        // DirectComposition visual tree: device -> target(hwnd) -> visual(swapchain).
        // SAFETY: `dxgi` is live; the out-param is a valid Option slot.
        let dcomp_device: IDCompositionDevice =
            unsafe { DCompositionCreateDevice(&dxgi)? };
        // SAFETY: `hwnd` is a live top-level window. `true` makes this the
        // topmost target for that window.
        let dcomp_target = unsafe { dcomp_device.CreateTargetForHwnd(hwnd, true)? };
        // SAFETY: device is live.
        let dcomp_visual = unsafe { dcomp_device.CreateVisual()? };
        // SAFETY: all three objects are live and owned by this struct.
        unsafe {
            dcomp_visual.SetContent(&swapchain)?;
            dcomp_target.SetRoot(&dcomp_visual)?;
            dcomp_device.Commit()?;
        }

        let mut me = Self {
            _d3d: d3d,
            _d2d_device: d2d_device,
            ctx,
            swapchain,
            _dcomp_device: dcomp_device,
            _dcomp_target: dcomp_target,
            _dcomp_visual: dcomp_visual,
            target_bitmap: None,
            kind,
            size: (w, h),
            dpi_scale,
        };
        me.bind_target()?;
        Ok(me)
    }

    pub fn kind(&self) -> DriverKind {
        self.kind
    }

    pub fn size(&self) -> (u32, u32) {
        self.size
    }

    pub fn dpi_scale(&self) -> f32 {
        self.dpi_scale
    }

    pub fn set_dpi_scale(&mut self, scale: f32) {
        self.dpi_scale = scale;
        let dpi = scale * 96.0;
        // SAFETY: context is live; SetDpi takes plain floats.
        unsafe { self.ctx.SetDpi(dpi, dpi) };
    }

    /// Point the D2D context at the swapchain's current back buffer.
    fn bind_target(&mut self) -> Result<(), RenderError> {
        // SAFETY: swapchain is live; buffer 0 always exists on a flip-model
        // chain with BufferCount >= 1.
        let surface: IDXGISurface = unsafe { self.swapchain.GetBuffer(0)? };

        let dpi = self.dpi_scale * 96.0;
        let props = D2D1_BITMAP_PROPERTIES1 {
            pixelFormat: D2D1_PIXEL_FORMAT {
                format: DXGI_FORMAT_B8G8R8A8_UNORM,
                alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
            },
            dpiX: dpi,
            dpiY: dpi,
            bitmapOptions: D2D1_BITMAP_OPTIONS_TARGET | D2D1_BITMAP_OPTIONS_CANNOT_DRAW,
            colorContext: core::mem::ManuallyDrop::new(None),
        };

        // SAFETY: `surface` is the live back buffer and `props` matches its
        // format exactly; a mismatch here is what produces E_INVALIDARG.
        let bmp = unsafe { self.ctx.CreateBitmapFromDxgiSurface(&surface, Some(&props))? };
        // SAFETY: context and bitmap are both live and owned here.
        unsafe { self.ctx.SetTarget(&bmp) };
        self.target_bitmap = Some(bmp);
        // SAFETY: plain float arguments.
        unsafe { self.ctx.SetDpi(dpi, dpi) };
        Ok(())
    }

    pub fn resize(&mut self, w: u32, h: u32) -> Result<(), RenderError> {
        let (w, h) = (w.max(1), h.max(1));
        if (w, h) == self.size {
            return Ok(());
        }
        // The target must be released before ResizeBuffers, or the swapchain
        // still has an outstanding reference and the call fails.
        // SAFETY: context is live; clearing the target is always valid.
        unsafe { self.ctx.SetTarget(None) };
        self.target_bitmap = None;

        // SAFETY: swapchain is live; passing 0 for count/format preserves the
        // existing description.
        unsafe {
            self.swapchain
                .ResizeBuffers(0, w, h, DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SWAP_CHAIN_FLAG(0))?;
        }
        self.size = (w, h);
        self.bind_target()
    }

    /// Rebuild everything after device loss.
    pub fn recreate(&mut self, hwnd: HWND) -> Result<(), RenderError> {
        let fresh = Self::new(hwnd, self.kind, self.size, self.dpi_scale)?;
        *self = fresh;
        Ok(())
    }

    pub fn begin_draw(&self) {
        // SAFETY: context is live with a bound target.
        unsafe { self.ctx.BeginDraw() };
    }

    pub fn end_draw(&self) -> Result<(), RenderError> {
        // SAFETY: paired with begin_draw on a live context.
        unsafe { self.ctx.EndDraw(None, None)? };
        Ok(())
    }

    pub fn present(&self) -> Result<(), RenderError> {
        // SAFETY: swapchain is live. Sync interval 1 avoids tearing; this is a
        // render-on-demand UI, so it presents only when something changed.
        unsafe { self.swapchain.Present(1, Default::default()).ok()? };
        // SAFETY: committing the visual tree publishes the frame.
        unsafe { self._dcomp_device.Commit()? };
        Ok(())
    }
}

/// Create a D3D11 device and a D2D device context with no swapchain.
///
/// This is the path the golden-image suite uses: rendering offscreen on WARP
/// needs the D2D context but not a window, a swapchain, or a visual tree.
pub fn create_context(kind: DriverKind) -> Result<(ID3D11Device, ID2D1DeviceContext), RenderError> {
    let driver = match kind {
        DriverKind::Hardware => D3D_DRIVER_TYPE_HARDWARE,
        DriverKind::Warp => D3D_DRIVER_TYPE_WARP,
    };
    let mut d3d: Option<ID3D11Device> = None;
    // SAFETY: out-params are valid Option slots; BGRA_SUPPORT is required for
    // D2D interop.
    unsafe {
        D3D11CreateDevice(
            None,
            driver,
            HMODULE::default(),
            D3D11_CREATE_DEVICE_BGRA_SUPPORT,
            None,
            D3D11_SDK_VERSION,
            Some(&mut d3d),
            None,
            None,
        )?;
    }
    let d3d = d3d.ok_or_else(|| RenderError(windows::core::Error::from(E_FAIL)))?;
    let dxgi: IDXGIDevice = d3d.cast()?;
    // SAFETY: creates the factory; the annotation selects the interface.
    let factory: ID2D1Factory1 =
        unsafe { D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)? };
    // SAFETY: `dxgi` is a live device we own.
    let device = unsafe { factory.CreateDevice(&dxgi)? };
    // SAFETY: single-threaded context on a device we own.
    let ctx = unsafe { device.CreateDeviceContext(D2D1_DEVICE_CONTEXT_OPTIONS_NONE)? };
    Ok((d3d, ctx))
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    #[test]
    fn warp_context_can_be_created_without_a_window() {
        // The golden-image suite depends on this: WARP is bit-identical across
        // machines, which is the only thing that makes exact pixel comparison
        // meaningful rather than flaky.
        let r = create_context(DriverKind::Warp);
        assert!(r.is_ok(), "WARP context creation failed: {:?}", r.err());
    }

    #[test]
    fn hardware_context_can_be_created() {
        let r = create_context(DriverKind::Hardware);
        assert!(r.is_ok(), "hardware context creation failed: {:?}", r.err());
    }

    #[test]
    fn device_lost_codes_are_recognised() {
        for hr in [
            DXGI_ERROR_DEVICE_REMOVED,
            DXGI_ERROR_DEVICE_RESET,
            DXGI_ERROR_DEVICE_HUNG,
            DXGI_ERROR_DRIVER_INTERNAL_ERROR,
            D2DERR_RECREATE_TARGET,
        ] {
            assert!(
                is_device_lost(&windows::core::Error::from(hr)),
                "{hr:?} should be treated as device-lost"
            );
        }
    }

    #[test]
    fn ordinary_errors_are_not_mistaken_for_device_loss() {
        // Rebuilding the whole stack on an unrelated failure would mask real
        // bugs behind an infinite recreate loop.
        assert!(!is_device_lost(&windows::core::Error::from(E_FAIL)));
    }
}
