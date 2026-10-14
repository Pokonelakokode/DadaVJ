//! CVPixelBuffer -> wgpu textúra **zero-copy** úton.
//!
//! A lánc:
//!   CVPixelBuffer (VideoToolbox, IOSurface-hátú)
//!     -> CVMetalTextureCache -> MTLTexture        (CoreVideo, GPU-oldali kötés)
//!     -> wgpu-hal texture_from_raw                (hal szintű wrapper, nem másol)
//!     -> wgpu::Device::create_texture_from_hal    (wgpu textúra, ugyanaz a memória)
//!
//! Ez az út **nem másolja a képet a CPU-n**, és nem is másolja GPU-oldalon sem:
//! a shader közvetlenül a dekóder IOSurface-ének MTLTexture-jét mintavételezi.

use anyhow::{anyhow, bail, Result};
use std::ptr::NonNull;

use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2_core_foundation::CFRetained;
use objc2_core_video::{CVMetalTexture, CVMetalTextureCache, CVMetalTextureGetTexture, CVPixelBuffer};
use objc2_metal::{MTLDevice, MTLPixelFormat};

/// Létrehoz egy CVMetalTexture-t a pixel buffer adott síkjából.
fn create_cv_metal_texture(
    cache: &CVMetalTextureCache,
    pixel_buffer: &CVPixelBuffer,
    pixel_format: MTLPixelFormat,
    width: usize,
    height: usize,
    plane: usize,
) -> Result<CFRetained<CVMetalTexture>> {
    let mut out: *mut CVMetalTexture = std::ptr::null_mut();
    let ret = unsafe {
        CVMetalTextureCache::create_texture_from_image(
            None,
            cache,
            pixel_buffer,
            None,
            pixel_format,
            width,
            height,
            plane,
            NonNull::new_unchecked(&mut out),
        )
    };
    if ret != 0 {
        bail!("CVMetalTextureCacheCreateTextureFromImage hiba: {ret}");
    }
    let ptr = NonNull::new(out).ok_or_else(|| anyhow!("üres CVMetalTexture"))?;
    Ok(unsafe { CFRetained::from_raw(ptr) })
}

/// A Metal textúra-cache, ami a wgpu device-hez kötődik.
pub struct MetalTextureCache {
    cache: CFRetained<CVMetalTextureCache>,
    _device: metal::Device,
}

impl MetalTextureCache {
    pub fn new(device: &wgpu::Device) -> Result<Self> {
        use metal::foreign_types::ForeignType;

        let mtl_device = unsafe {
            device.as_hal::<wgpu::hal::api::Metal, _, _>(|d| {
                d.map(|d| d.raw_device().lock().clone())
            })
        }
        .ok_or_else(|| anyhow!("nincs Metal hal device"))?;

        let raw_ptr = mtl_device.as_ptr();
        let objc_device: &ProtocolObject<dyn MTLDevice> =
            unsafe { &*(raw_ptr as *const ProtocolObject<dyn MTLDevice>) };

        let mut cache_ptr: *mut CVMetalTextureCache = std::ptr::null_mut();
        let ret = unsafe {
            CVMetalTextureCache::create(
                None,
                None,
                objc_device,
                None,
                NonNull::new_unchecked(&mut cache_ptr),
            )
        };
        if ret != 0 {
            bail!("CVMetalTextureCacheCreate hiba: {ret}");
        }
        let ptr = NonNull::new(cache_ptr).ok_or_else(|| anyhow!("üres CVMetalTextureCache"))?;
        let cache = unsafe { CFRetained::from_raw(ptr) };

        Ok(Self {
            cache,
            _device: mtl_device,
        })
    }

    /// BGRA8 pixelt tartalmazó pixel buffer becsomagolása wgpu textúrává.
    ///
    /// A visszaadott CVMetalTexture-et **életben kell tartani**, amíg a GPU
    /// használja a textúrát (a binding a pixel bufferhez köti).
    pub fn wrap_bgra(
        &self,
        device: &wgpu::Device,
        pixel_buffer: &CVPixelBuffer,
        width: u32,
        height: u32,
    ) -> Result<(wgpu::Texture, CFRetained<CVMetalTexture>)> {
        let cv_texture = create_cv_metal_texture(
            &self.cache,
            pixel_buffer,
            MTLPixelFormat::BGRA8Unorm,
            width as usize,
            height as usize,
            0,
        )?;

        let objc_texture = unsafe { CVMetalTextureGetTexture(&cv_texture) }
            .ok_or_else(|| anyhow!("CVMetalTextureGetTexture üres"))?;

        // A +1 referenciát átadjuk a metal-rs Textúrának (nem duplán szabadítjuk fel).
        use metal::foreign_types::ForeignType;
        let raw = Retained::into_raw(objc_texture);
        let mtl_texture: metal::Texture =
            unsafe { metal::Texture::from_ptr(raw.cast::<metal::MTLTexture>()) };

        let descriptor = wgpu::TextureDescriptor {
            label: Some("video-frame"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Bgra8UnormSrgb,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        };

        let texture = unsafe {
            device.as_hal::<wgpu::hal::api::Metal, _, _>(|hal| {
                let hal_device = hal.ok_or_else(|| anyhow!("nincs hal device"))?;
                let _ = hal_device;
                let hal_texture = wgpu::hal::metal::Device::texture_from_raw(
                    mtl_texture,
                    wgpu::TextureFormat::Bgra8UnormSrgb,
                    metal::MTLTextureType::D2,
                    1,
                    1,
                    wgpu::hal::CopyExtent {
                        width,
                        height,
                        depth: 1,
                    },
                );
                Ok::<_, anyhow::Error>(device.create_texture_from_hal::<wgpu::hal::api::Metal>(
                    hal_texture,
                    &descriptor,
                ))
            })
        }?;

        Ok((texture, cv_texture))
    }
}
