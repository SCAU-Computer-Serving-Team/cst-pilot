#[cfg(windows)]
use std::collections::VecDeque;
#[cfg(windows)]
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

#[cfg(windows)]
use anyhow::{Context as _, Result};
#[cfg(windows)]
use gpui::{WindowsSurfaceDrawContext, WindowsSurfaceRenderer};

use crate::background::presets::{
    BlueHourUniforms, FrostedGlassSpec, FrostedGlassUniforms, SkyPreset, SkyUniforms,
    decode_palette_256x1, generate_rounded_rect_sdf_rgba16f,
};
#[cfg(windows)]
use crate::shaders::{
    PS_BLUE_HOUR_DXBC, PS_BLUE_SKY_DXBC, PS_FROSTED_GLASS_DXBC, PS_NIGHT_SKY_DXBC, VS_SURFACE_DXBC,
};

/// Minimum Direct3D feature level (`11_0` = `0xb000`) required for `ps_5_0` shaders.
pub const D3D_FEATURE_LEVEL_11_0_RAW: u32 = 0xb000;

static SUPPORTS_SM50: AtomicBool = AtomicBool::new(true);
static IS_SOFTWARE_ADAPTER: AtomicBool = AtomicBool::new(false);
static SKY_DRAW_COUNT: AtomicU64 = AtomicU64::new(0);
static FROSTED_DRAW_COUNT: AtomicU64 = AtomicU64::new(0);
static DEVICE_BUILD_COUNT: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SurfaceRuntimeDiagnostics {
    pub supports_sm50: bool,
    pub is_software_adapter: bool,
    pub sky_draw_count: u64,
    pub frosted_draw_count: u64,
    pub device_build_count: u64,
}

pub fn surface_diagnostics() -> SurfaceRuntimeDiagnostics {
    let forced_warp = crate::ux_probe::forced_warp_enabled();
    SurfaceRuntimeDiagnostics {
        supports_sm50: !forced_warp && SUPPORTS_SM50.load(Ordering::Relaxed),
        is_software_adapter: forced_warp || IS_SOFTWARE_ADAPTER.load(Ordering::Relaxed),
        sky_draw_count: SKY_DRAW_COUNT.load(Ordering::Relaxed),
        frosted_draw_count: FROSTED_DRAW_COUNT.load(Ordering::Relaxed),
        device_build_count: DEVICE_BUILD_COUNT.load(Ordering::Relaxed),
    }
}

pub fn d3d_sm50_supported() -> bool {
    !crate::ux_probe::forced_warp_enabled() && SUPPORTS_SM50.load(Ordering::Relaxed)
}

pub fn d3d_is_software_adapter() -> bool {
    crate::ux_probe::forced_warp_enabled() || IS_SOFTWARE_ADAPTER.load(Ordering::Relaxed)
}

/// Explicitly drops cached D3D11 pipeline objects so the next frame rebuilds them cleanly
/// (used in device-recovery tests and adapter resets).
pub fn reset_d3d11_pipeline_cache() {
    #[cfg(windows)]
    {
        if let Ok(mut guard) = PIPELINE_CACHE.lock() {
            *guard = None;
        }
    }
}

#[cfg(windows)]
mod imp {
    use super::*;
    use std::slice;
    use windows::Win32::Foundation::RECT;
    use windows::Win32::Graphics::Direct3D::D3D_PRIMITIVE_TOPOLOGY_TRIANGLESTRIP;
    use windows::Win32::Graphics::Direct3D11::{
        D3D11_BIND_CONSTANT_BUFFER, D3D11_BIND_SHADER_RESOURCE, D3D11_BLEND_DESC,
        D3D11_BLEND_INV_SRC_ALPHA, D3D11_BLEND_ONE, D3D11_BLEND_OP_ADD, D3D11_BLEND_SRC_ALPHA,
        D3D11_BOX, D3D11_BUFFER_DESC, D3D11_COLOR_WRITE_ENABLE_ALL, D3D11_COMPARISON_NEVER,
        D3D11_CPU_ACCESS_WRITE, D3D11_CULL_NONE, D3D11_FILL_SOLID, D3D11_FILTER_MIN_MAG_MIP_LINEAR,
        D3D11_MAP_WRITE_DISCARD, D3D11_RASTERIZER_DESC, D3D11_RENDER_TARGET_BLEND_DESC,
        D3D11_SAMPLER_DESC, D3D11_SUBRESOURCE_DATA, D3D11_TEXTURE_ADDRESS_CLAMP,
        D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT, D3D11_USAGE_DYNAMIC, D3D11_USAGE_IMMUTABLE,
        ID3D11BlendState, ID3D11Buffer, ID3D11Device, ID3D11DeviceContext, ID3D11PixelShader,
        ID3D11RasterizerState, ID3D11SamplerState, ID3D11ShaderResourceView, ID3D11Texture2D,
        ID3D11VertexShader,
    };
    use windows::Win32::Graphics::Dxgi::Common::{
        DXGI_FORMAT, DXGI_FORMAT_R8G8B8A8_UNORM, DXGI_FORMAT_R16G16B16A16_FLOAT, DXGI_SAMPLE_DESC,
    };
    use windows::core::Interface;

    const MAX_CACHED_SDFS: usize = 16;

    #[derive(Clone, Copy, PartialEq, Eq)]
    struct SdfCacheKey {
        width: u32,
        height: u32,
        radius_tenths: u32,
    }

    pub(super) struct DevicePipelines {
        device_ptr: usize,
        vs: ID3D11VertexShader,
        ps_blue_hour: ID3D11PixelShader,
        ps_blue_sky: ID3D11PixelShader,
        ps_night_sky: ID3D11PixelShader,
        ps_frosted_glass: ID3D11PixelShader,
        cbuffer_96: ID3D11Buffer,
        sampler_linear_clamp: ID3D11SamplerState,
        rasterizer_scissor: ID3D11RasterizerState,
        blend_alpha: ID3D11BlendState,
        palette_light_srv: ID3D11ShaderResourceView,
        palette_dark_srv: ID3D11ShaderResourceView,
        backdrop_tex: Option<(
            u32,
            u32,
            DXGI_FORMAT,
            ID3D11Texture2D,
            ID3D11ShaderResourceView,
        )>,
        sdf_cache: VecDeque<(SdfCacheKey, ID3D11ShaderResourceView)>,
    }

    // SAFETY: D3D11 resource handles in `DevicePipelines` are only mutated under `PIPELINE_CACHE` Mutex
    // and bound on GPUI's single render thread for the matching `device_ptr`.
    unsafe impl Send for DevicePipelines {}

    pub(super) static PIPELINE_CACHE: Mutex<Option<DevicePipelines>> = Mutex::new(None);

    impl DevicePipelines {
        fn build(device: &ID3D11Device, device_ptr: usize) -> Result<Self> {
            unsafe {
                let mut vs = None;
                device
                    .CreateVertexShader(VS_SURFACE_DXBC, None, Some(&mut vs))
                    .context("CreateVertexShader(VS_SURFACE_DXBC)")?;

                let mut ps_blue_hour = None;
                device
                    .CreatePixelShader(PS_BLUE_HOUR_DXBC, None, Some(&mut ps_blue_hour))
                    .context("CreatePixelShader(PS_BLUE_HOUR_DXBC)")?;

                let mut ps_blue_sky = None;
                device
                    .CreatePixelShader(PS_BLUE_SKY_DXBC, None, Some(&mut ps_blue_sky))
                    .context("CreatePixelShader(PS_BLUE_SKY_DXBC)")?;

                let mut ps_night_sky = None;
                device
                    .CreatePixelShader(PS_NIGHT_SKY_DXBC, None, Some(&mut ps_night_sky))
                    .context("CreatePixelShader(PS_NIGHT_SKY_DXBC)")?;

                let mut ps_frosted_glass = None;
                device
                    .CreatePixelShader(PS_FROSTED_GLASS_DXBC, None, Some(&mut ps_frosted_glass))
                    .context("CreatePixelShader(PS_FROSTED_GLASS_DXBC)")?;

                let cb_desc = D3D11_BUFFER_DESC {
                    ByteWidth: 96,
                    Usage: D3D11_USAGE_DYNAMIC,
                    BindFlags: D3D11_BIND_CONSTANT_BUFFER.0 as u32,
                    CPUAccessFlags: D3D11_CPU_ACCESS_WRITE.0 as u32,
                    MiscFlags: 0,
                    StructureByteStride: 0,
                };
                let mut cbuffer_96 = None;
                device
                    .CreateBuffer(&cb_desc, None, Some(&mut cbuffer_96))
                    .context("CreateBuffer(cbuffer_96)")?;

                let samp_desc = D3D11_SAMPLER_DESC {
                    Filter: D3D11_FILTER_MIN_MAG_MIP_LINEAR,
                    AddressU: D3D11_TEXTURE_ADDRESS_CLAMP,
                    AddressV: D3D11_TEXTURE_ADDRESS_CLAMP,
                    AddressW: D3D11_TEXTURE_ADDRESS_CLAMP,
                    MipLODBias: 0.0,
                    MaxAnisotropy: 1,
                    ComparisonFunc: D3D11_COMPARISON_NEVER,
                    BorderColor: [0.0; 4],
                    MinLOD: 0.0,
                    MaxLOD: f32::MAX,
                };
                let mut sampler_linear_clamp = None;
                device
                    .CreateSamplerState(&samp_desc, Some(&mut sampler_linear_clamp))
                    .context("CreateSamplerState")?;

                let rast_desc = D3D11_RASTERIZER_DESC {
                    FillMode: D3D11_FILL_SOLID,
                    CullMode: D3D11_CULL_NONE,
                    ScissorEnable: true.into(),
                    ..Default::default()
                };
                let mut rasterizer_scissor = None;
                device
                    .CreateRasterizerState(&rast_desc, Some(&mut rasterizer_scissor))
                    .context("CreateRasterizerState")?;

                let mut blend_desc = D3D11_BLEND_DESC::default();
                blend_desc.RenderTarget[0] = D3D11_RENDER_TARGET_BLEND_DESC {
                    BlendEnable: true.into(),
                    SrcBlend: D3D11_BLEND_SRC_ALPHA,
                    DestBlend: D3D11_BLEND_INV_SRC_ALPHA,
                    BlendOp: D3D11_BLEND_OP_ADD,
                    SrcBlendAlpha: D3D11_BLEND_ONE,
                    DestBlendAlpha: D3D11_BLEND_INV_SRC_ALPHA,
                    BlendOpAlpha: D3D11_BLEND_OP_ADD,
                    RenderTargetWriteMask: D3D11_COLOR_WRITE_ENABLE_ALL.0 as u8,
                };
                let mut blend_alpha = None;
                device
                    .CreateBlendState(&blend_desc, Some(&mut blend_alpha))
                    .context("CreateBlendState")?;

                let palette_light_srv = create_palette_srv(device, decode_palette_256x1(true))?;
                let palette_dark_srv = create_palette_srv(device, decode_palette_256x1(false))?;

                DEVICE_BUILD_COUNT.fetch_add(1, Ordering::Relaxed);

                Ok(Self {
                    device_ptr,
                    vs: vs.unwrap(),
                    ps_blue_hour: ps_blue_hour.unwrap(),
                    ps_blue_sky: ps_blue_sky.unwrap(),
                    ps_night_sky: ps_night_sky.unwrap(),
                    ps_frosted_glass: ps_frosted_glass.unwrap(),
                    cbuffer_96: cbuffer_96.unwrap(),
                    sampler_linear_clamp: sampler_linear_clamp.unwrap(),
                    rasterizer_scissor: rasterizer_scissor.unwrap(),
                    blend_alpha: blend_alpha.unwrap(),
                    palette_light_srv,
                    palette_dark_srv,
                    backdrop_tex: None,
                    sdf_cache: VecDeque::with_capacity(MAX_CACHED_SDFS),
                })
            }
        }

        fn ensure_backdrop(
            &mut self,
            device: &ID3D11Device,
            min_w: u32,
            min_h: u32,
            format: DXGI_FORMAT,
        ) -> Result<(u32, u32, ID3D11Texture2D, ID3D11ShaderResourceView)> {
            if let Some((aw, ah, afmt, ref tex, ref srv)) = self.backdrop_tex
                && aw >= min_w
                && ah >= min_h
                && afmt == format
            {
                return Ok((aw, ah, tex.clone(), srv.clone()));
            }
            let alloc_w = min_w.max(64).div_ceil(256) * 256;
            let alloc_h = min_h.max(64).div_ceil(256) * 256;
            let desc = D3D11_TEXTURE2D_DESC {
                Width: alloc_w,
                Height: alloc_h,
                MipLevels: 1,
                ArraySize: 1,
                Format: format,
                SampleDesc: DXGI_SAMPLE_DESC {
                    Count: 1,
                    Quality: 0,
                },
                Usage: D3D11_USAGE_DEFAULT,
                BindFlags: D3D11_BIND_SHADER_RESOURCE.0 as u32,
                CPUAccessFlags: 0,
                MiscFlags: 0,
            };
            unsafe {
                let mut tex = None;
                device
                    .CreateTexture2D(&desc, None, Some(&mut tex))
                    .context("CreateTexture2D(backdrop)")?;
                let tex = tex.unwrap();
                let mut srv = None;
                device
                    .CreateShaderResourceView(&tex, None, Some(&mut srv))
                    .context("CreateShaderResourceView(backdrop)")?;
                let srv = srv.unwrap();
                self.backdrop_tex = Some((alloc_w, alloc_h, format, tex.clone(), srv.clone()));
                Ok((alloc_w, alloc_h, tex, srv))
            }
        }

        fn ensure_sdf_srv(
            &mut self,
            device: &ID3D11Device,
            width: u32,
            height: u32,
            radius_px: f32,
        ) -> Result<ID3D11ShaderResourceView> {
            let key = SdfCacheKey {
                width: width.max(1),
                height: height.max(1),
                radius_tenths: (radius_px.max(0.0) * 10.0).round() as u32,
            };
            if let Some(pos) = self.sdf_cache.iter().position(|(k, _)| *k == key) {
                let entry = self.sdf_cache.remove(pos).unwrap();
                let srv = entry.1.clone();
                self.sdf_cache.push_back(entry);
                return Ok(srv);
            }

            let texels = generate_rounded_rect_sdf_rgba16f(key.width, key.height, radius_px);
            let desc = D3D11_TEXTURE2D_DESC {
                Width: key.width,
                Height: key.height,
                MipLevels: 1,
                ArraySize: 1,
                Format: DXGI_FORMAT_R16G16B16A16_FLOAT,
                SampleDesc: DXGI_SAMPLE_DESC {
                    Count: 1,
                    Quality: 0,
                },
                Usage: D3D11_USAGE_IMMUTABLE,
                BindFlags: D3D11_BIND_SHADER_RESOURCE.0 as u32,
                CPUAccessFlags: 0,
                MiscFlags: 0,
            };
            let init = D3D11_SUBRESOURCE_DATA {
                pSysMem: texels.as_ptr().cast(),
                SysMemPitch: key.width * 8,
                SysMemSlicePitch: 0,
            };
            unsafe {
                let mut tex = None;
                device
                    .CreateTexture2D(&desc, Some(&init), Some(&mut tex))
                    .context("CreateTexture2D(sdf_rgba16f)")?;
                let tex = tex.unwrap();
                let mut srv = None;
                device
                    .CreateShaderResourceView(&tex, None, Some(&mut srv))
                    .context("CreateShaderResourceView(sdf_rgba16f)")?;
                let srv = srv.unwrap();
                if self.sdf_cache.len() >= MAX_CACHED_SDFS {
                    self.sdf_cache.pop_front();
                }
                self.sdf_cache.push_back((key, srv.clone()));
                Ok(srv)
            }
        }
    }

    unsafe fn create_palette_srv(
        device: &ID3D11Device,
        row: &[[u8; 4]; 256],
    ) -> Result<ID3D11ShaderResourceView> {
        let desc = D3D11_TEXTURE2D_DESC {
            Width: 256,
            Height: 1,
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT_R8G8B8A8_UNORM,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            Usage: D3D11_USAGE_IMMUTABLE,
            BindFlags: D3D11_BIND_SHADER_RESOURCE.0 as u32,
            CPUAccessFlags: 0,
            MiscFlags: 0,
        };
        let init = D3D11_SUBRESOURCE_DATA {
            pSysMem: row.as_ptr().cast(),
            SysMemPitch: 256 * 4,
            SysMemSlicePitch: 0,
        };
        unsafe {
            let mut tex = None;
            device
                .CreateTexture2D(&desc, Some(&init), Some(&mut tex))
                .context("CreateTexture2D(palette)")?;
            let tex = tex.unwrap();
            let mut srv = None;
            device
                .CreateShaderResourceView(&tex, None, Some(&mut srv))
                .context("CreateShaderResourceView(palette)")?;
            Ok(srv.unwrap())
        }
    }

    unsafe fn write_cbuffer_bytes(
        ctx: &ID3D11DeviceContext,
        buffer: &ID3D11Buffer,
        bytes: &[u8],
    ) -> Result<()> {
        debug_assert!(bytes.len() <= 96);
        unsafe {
            let mut mapped = std::mem::zeroed();
            ctx.Map(buffer, 0, D3D11_MAP_WRITE_DISCARD, 0, Some(&mut mapped))
                .context("Map(cbuffer_96)")?;
            let dst = mapped.pData.cast::<u8>();
            std::ptr::write_bytes(dst, 0, 96);
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), dst, bytes.len());
            ctx.Unmap(buffer, 0);
        }
        Ok(())
    }

    fn compute_scissor_rect(ctx: &WindowsSurfaceDrawContext) -> Option<RECT> {
        let left = ctx
            .bounds
            .origin
            .x
            .0
            .max(ctx.content_mask.bounds.origin.x.0)
            .max(0.0)
            .floor() as i32;
        let top = ctx
            .bounds
            .origin
            .y
            .0
            .max(ctx.content_mask.bounds.origin.y.0)
            .max(0.0)
            .floor() as i32;
        let right = (ctx.bounds.origin.x.0 + ctx.bounds.size.width.0)
            .min(ctx.content_mask.bounds.origin.x.0 + ctx.content_mask.bounds.size.width.0)
            .min(ctx.viewport_size[0])
            .ceil() as i32;
        let bottom = (ctx.bounds.origin.y.0 + ctx.bounds.size.height.0)
            .min(ctx.content_mask.bounds.origin.y.0 + ctx.content_mask.bounds.size.height.0)
            .min(ctx.viewport_size[1])
            .ceil() as i32;
        if right <= left || bottom <= top {
            None
        } else {
            Some(RECT {
                left,
                top,
                right,
                bottom,
            })
        }
    }

    pub(super) fn paint_sky(
        ctx: &WindowsSurfaceDrawContext,
        preset: SkyPreset,
        time_sec: f32,
        opacity: f32,
        corner_radii_phys: [f32; 4],
    ) -> Result<()> {
        IS_SOFTWARE_ADAPTER.store(ctx.is_software_emulated, Ordering::Relaxed);
        if ctx.feature_level < D3D_FEATURE_LEVEL_11_0_RAW {
            SUPPORTS_SM50.store(false, Ordering::Relaxed);
            return Ok(());
        }
        SUPPORTS_SM50.store(true, Ordering::Relaxed);

        let Some(scissor) = compute_scissor_rect(ctx) else {
            return Ok(());
        };
        let Some(device) = (unsafe { ID3D11Device::from_raw_borrowed(&ctx.device_raw) }) else {
            return Ok(());
        };
        let Some(d3d_ctx) =
            (unsafe { ID3D11DeviceContext::from_raw_borrowed(&ctx.device_context_raw) })
        else {
            return Ok(());
        };

        let mut cache_guard = PIPELINE_CACHE.lock().expect("d3d11 pipeline lock");
        let dev_ptr = ctx.device_raw as usize;
        if cache_guard.as_ref().is_none_or(|p| p.device_ptr != dev_ptr) {
            *cache_guard = Some(DevicePipelines::build(device, dev_ptr)?);
        }
        let pipes = cache_guard.as_ref().expect("built pipelines");

        let origin = [ctx.bounds.origin.x.0, ctx.bounds.origin.y.0];
        let res = [
            ctx.bounds.size.width.0.max(1.0),
            ctx.bounds.size.height.0.max(1.0),
        ];
        let effective_time = if ctx.is_software_emulated {
            0.0
        } else {
            time_sec
        };

        unsafe {
            match preset {
                SkyPreset::HomeLight | SkyPreset::HomeDark => {
                    let u = BlueHourUniforms::new(
                        origin,
                        res,
                        ctx.viewport_size,
                        opacity,
                        effective_time,
                        corner_radii_phys,
                    );
                    let raw = slice::from_raw_parts(
                        (&u as *const BlueHourUniforms).cast::<u8>(),
                        std::mem::size_of::<BlueHourUniforms>(),
                    );
                    write_cbuffer_bytes(d3d_ctx, &pipes.cbuffer_96, raw)?;
                    let srv = if matches!(preset, SkyPreset::HomeLight) {
                        Some(pipes.palette_light_srv.clone())
                    } else {
                        Some(pipes.palette_dark_srv.clone())
                    };
                    d3d_ctx.PSSetShader(&pipes.ps_blue_hour, None);
                    d3d_ctx.PSSetShaderResources(0, Some(slice::from_ref(&srv)));
                }
                SkyPreset::LoginLight | SkyPreset::LoginDark => {
                    let u = SkyUniforms::for_preset(
                        preset,
                        origin,
                        res,
                        ctx.viewport_size,
                        opacity,
                        effective_time,
                        corner_radii_phys,
                    );
                    let raw = slice::from_raw_parts(
                        (&u as *const SkyUniforms).cast::<u8>(),
                        std::mem::size_of::<SkyUniforms>(),
                    );
                    write_cbuffer_bytes(d3d_ctx, &pipes.cbuffer_96, raw)?;
                    let ps = if matches!(preset, SkyPreset::LoginLight) {
                        &pipes.ps_blue_sky
                    } else {
                        &pipes.ps_night_sky
                    };
                    d3d_ctx.PSSetShader(ps, None);
                }
            }

            let cb = Some(pipes.cbuffer_96.clone());
            let smp = Some(pipes.sampler_linear_clamp.clone());
            d3d_ctx.IASetInputLayout(None);
            d3d_ctx.IASetPrimitiveTopology(D3D_PRIMITIVE_TOPOLOGY_TRIANGLESTRIP);
            d3d_ctx.VSSetShader(&pipes.vs, None);
            d3d_ctx.VSSetConstantBuffers(0, Some(slice::from_ref(&cb)));
            d3d_ctx.PSSetConstantBuffers(0, Some(slice::from_ref(&cb)));
            d3d_ctx.PSSetSamplers(0, Some(slice::from_ref(&smp)));
            d3d_ctx.RSSetState(&pipes.rasterizer_scissor);
            d3d_ctx.RSSetScissorRects(Some(slice::from_ref(&scissor)));
            d3d_ctx.OMSetBlendState(&pipes.blend_alpha, None, 0xFFFF_FFFF);
            d3d_ctx.Draw(4, 0);
        }

        SKY_DRAW_COUNT.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }

    pub(super) fn paint_frosted_glass(
        ctx: &WindowsSurfaceDrawContext,
        spec: &FrostedGlassSpec,
        scale_factor: f32,
    ) -> Result<()> {
        IS_SOFTWARE_ADAPTER.store(ctx.is_software_emulated, Ordering::Relaxed);
        if ctx.feature_level < D3D_FEATURE_LEVEL_11_0_RAW {
            SUPPORTS_SM50.store(false, Ordering::Relaxed);
            return Ok(());
        }
        SUPPORTS_SM50.store(true, Ordering::Relaxed);

        let Some(scissor) = compute_scissor_rect(ctx) else {
            return Ok(());
        };
        let Some(device) = (unsafe { ID3D11Device::from_raw_borrowed(&ctx.device_raw) }) else {
            return Ok(());
        };
        let Some(d3d_ctx) =
            (unsafe { ID3D11DeviceContext::from_raw_borrowed(&ctx.device_context_raw) })
        else {
            return Ok(());
        };
        let Some(render_target) =
            (unsafe { ID3D11Texture2D::from_raw_borrowed(&ctx.render_target_raw) })
        else {
            return Ok(());
        };

        let mut cache_guard = PIPELINE_CACHE.lock().expect("d3d11 pipeline lock");
        let dev_ptr = ctx.device_raw as usize;
        if cache_guard.as_ref().is_none_or(|p| p.device_ptr != dev_ptr) {
            *cache_guard = Some(DevicePipelines::build(device, dev_ptr)?);
        }
        let pipes = cache_guard.as_mut().expect("built pipelines");

        let vp_w = (ctx.viewport_size[0].round() as u32).max(1);
        let vp_h = (ctx.viewport_size[1].round() as u32).max(1);
        let scaled_blur = (spec.blur_px * scale_factor.max(1.0)).max(0.0);
        let pad = (scaled_blur.ceil() as i32).max(2);

        let bx0 = ctx.bounds.origin.x.0.floor() as i32;
        let by0 = ctx.bounds.origin.y.0.floor() as i32;
        let bx1 = (ctx.bounds.origin.x.0 + ctx.bounds.size.width.0).ceil() as i32;
        let by1 = (ctx.bounds.origin.y.0 + ctx.bounds.size.height.0).ceil() as i32;

        let cap_left = (bx0 - pad).clamp(0, vp_w as i32 - 1) as u32;
        let cap_top = (by0 - pad).clamp(0, vp_h as i32 - 1) as u32;
        let cap_right = (bx1 + pad).clamp(cap_left as i32 + 1, vp_w as i32) as u32;
        let cap_bottom = (by1 + pad).clamp(cap_top as i32 + 1, vp_h as i32) as u32;
        let cap_w = cap_right - cap_left;
        let cap_h = cap_bottom - cap_top;

        let mut rt_desc = D3D11_TEXTURE2D_DESC::default();
        unsafe { render_target.GetDesc(&mut rt_desc) };
        if rt_desc.SampleDesc.Count > 1 {
            return Ok(());
        }

        let (alloc_w, alloc_h, backdrop_tex, backdrop_srv) =
            pipes.ensure_backdrop(device, cap_w, cap_h, rt_desc.Format)?;

        // Copy live scene underneath the overlay into `backdrop_tex` before binding SRV
        let src_box = D3D11_BOX {
            left: cap_left,
            top: cap_top,
            front: 0,
            right: cap_right,
            bottom: cap_bottom,
            back: 1,
        };
        unsafe {
            d3d_ctx.CopySubresourceRegion(
                &backdrop_tex,
                0,
                0,
                0,
                0,
                render_target,
                0,
                Some(&src_box),
            );
        }

        let surf_w = (ctx.bounds.size.width.0.round() as u32).max(1);
        let surf_h = (ctx.bounds.size.height.0.round() as u32).max(1);
        let scaled_radius = spec.corner_radius_px * scale_factor.max(1.0);
        let sdf_srv = pipes.ensure_sdf_srv(device, surf_w, surf_h, scaled_radius)?;

        let u = FrostedGlassUniforms {
            slot0: [
                ctx.bounds.origin.x.0,
                ctx.bounds.origin.y.0,
                ctx.bounds.size.width.0.max(1.0),
                ctx.bounds.size.height.0.max(1.0),
            ],
            slot1: [
                ctx.viewport_size[0],
                ctx.viewport_size[1],
                spec.opacity,
                0.0,
            ],
            slot2: [
                scaled_blur,
                spec.refract_px * scale_factor.max(1.0),
                spec.vibrancy,
                spec.grain,
            ],
            slot3: [
                spec.tint_rgb[0],
                spec.tint_rgb[1],
                spec.tint_rgb[2],
                spec.tint_opacity,
            ],
            slot4: [
                spec.specular,
                ctx.bounds.origin.x.0 - cap_left as f32,
                ctx.bounds.origin.y.0 - cap_top as f32,
                0.0,
            ],
            slot5: [cap_w as f32, cap_h as f32, alloc_w as f32, alloc_h as f32],
        };

        unsafe {
            let raw = slice::from_raw_parts(
                (&u as *const FrostedGlassUniforms).cast::<u8>(),
                std::mem::size_of::<FrostedGlassUniforms>(),
            );
            write_cbuffer_bytes(d3d_ctx, &pipes.cbuffer_96, raw)?;

            let srvs = [Some(sdf_srv), Some(backdrop_srv)];
            let smps = [
                Some(pipes.sampler_linear_clamp.clone()),
                Some(pipes.sampler_linear_clamp.clone()),
            ];
            let cb = Some(pipes.cbuffer_96.clone());

            d3d_ctx.IASetInputLayout(None);
            d3d_ctx.IASetPrimitiveTopology(D3D_PRIMITIVE_TOPOLOGY_TRIANGLESTRIP);
            d3d_ctx.VSSetShader(&pipes.vs, None);
            d3d_ctx.PSSetShader(&pipes.ps_frosted_glass, None);
            d3d_ctx.VSSetConstantBuffers(0, Some(slice::from_ref(&cb)));
            d3d_ctx.PSSetConstantBuffers(0, Some(slice::from_ref(&cb)));
            d3d_ctx.PSSetShaderResources(0, Some(&srvs));
            d3d_ctx.PSSetSamplers(0, Some(&smps));
            d3d_ctx.RSSetState(&pipes.rasterizer_scissor);
            d3d_ctx.RSSetScissorRects(Some(slice::from_ref(&scissor)));
            d3d_ctx.OMSetBlendState(&pipes.blend_alpha, None, 0xFFFF_FFFF);
            d3d_ctx.Draw(4, 0);
        }

        FROSTED_DRAW_COUNT.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }
}

#[cfg(windows)]
use imp::PIPELINE_CACHE;

#[derive(Debug, Clone)]
pub struct D3d11SkySurface {
    pub preset: SkyPreset,
    pub time_sec: f32,
    pub opacity: f32,
    pub corner_radii_phys: [f32; 4],
}

#[cfg(windows)]
impl WindowsSurfaceRenderer for D3d11SkySurface {
    fn paint(&self, ctx: &WindowsSurfaceDrawContext) -> Result<()> {
        imp::paint_sky(
            ctx,
            self.preset,
            self.time_sec,
            self.opacity,
            self.corner_radii_phys,
        )
    }
}

#[derive(Debug, Clone)]
pub struct D3d11FrostedGlassSurface {
    pub spec: FrostedGlassSpec,
    pub scale_factor: f32,
}

#[cfg(windows)]
impl WindowsSurfaceRenderer for D3d11FrostedGlassSurface {
    fn paint(&self, ctx: &WindowsSurfaceDrawContext) -> Result<()> {
        imp::paint_frosted_glass(ctx, &self.spec, self.scale_factor)
    }
}
