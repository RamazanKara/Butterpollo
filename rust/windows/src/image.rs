//! Cover images: any format Windows can decode, written as PNG for Moonlight.
#![warn(clippy::undocumented_unsafe_blocks)]

use anyhow::{Context, Result};
use std::path::Path;
use windows::{
    Win32::{
        Foundation::GENERIC_READ,
        Graphics::Imaging::*,
        System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance},
    },
    core::HSTRING,
};

fn factory() -> Result<IWICImagingFactory> {
    // SAFETY: CoCreateInstance only reads the static CLSID, and callers hold a ComGuard.
    Ok(unsafe { CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER)? })
}
fn frame(factory: &IWICImagingFactory, path: &Path) -> Result<IWICBitmapFrameDecode> {
    // SAFETY: COM is initialised by the caller's ComGuard and the path HSTRING outlives the call.
    unsafe {
        let decoder = factory
            .CreateDecoderFromFilename(
                &HSTRING::from(path.as_os_str()),
                None,
                GENERIC_READ,
                WICDecodeMetadataCacheOnDemand,
            )
            .with_context(|| format!("decoding {}", path.display()))?;
        Ok(decoder.GetFrame(0)?)
    }
}
/// An image's width and height.
pub fn dimensions(path: &Path) -> Result<(u32, u32)> {
    let _com = crate::capture::ComGuard::new()?;
    let factory = factory()?;
    let frame = frame(&factory, path)?;
    let (mut width, mut height) = (0, 0);
    // SAFETY: COM is initialised by `_com`, and `width` and `height` are live out-pointers.
    unsafe { frame.GetSize(&mut width, &mut height)? };
    Ok((width, height))
}
/// Write `source` (JPEG, PNG, WebP with the Windows codec, ...) to
/// `destination` as a PNG, replacing it only once the new file is complete.
pub fn to_png(source: &Path, destination: &Path) -> Result<(u32, u32)> {
    let _com = crate::capture::ComGuard::new()?;
    let factory = factory()?;
    let frame = frame(&factory, source)?;
    let mut partial = destination.as_os_str().to_owned();
    partial.push(".partial");
    let partial = std::path::PathBuf::from(partial);
    if let Some(parent) = destination.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let result = (|| -> Result<(u32, u32)> {
        // SAFETY: COM is initialised by `_com` for the whole closure, every interface used is live,
        // and the null pointers passed are the documented "no options" values.
        unsafe {
            let (mut width, mut height) = (0, 0);
            frame.GetSize(&mut width, &mut height)?;
            let converter = factory.CreateFormatConverter()?;
            converter.Initialize(
                &frame,
                &GUID_WICPixelFormat32bppBGRA,
                WICBitmapDitherTypeNone,
                None,
                0.,
                WICBitmapPaletteTypeCustom,
            )?;
            let stream = factory.CreateStream()?;
            stream.InitializeFromFilename(&HSTRING::from(partial.as_os_str()), 0x4000_0000)?;
            let encoder = factory.CreateEncoder(&GUID_ContainerFormatPng, std::ptr::null())?;
            encoder.Initialize(&stream, WICBitmapEncoderNoCache)?;
            let mut target = None;
            let mut options = None;
            encoder.CreateNewFrame(&mut target, &mut options)?;
            let target = target.context("PNG encoder frame")?;
            target.Initialize(options.as_ref())?;
            target.SetSize(width, height)?;
            let mut format = GUID_WICPixelFormat32bppBGRA;
            target.SetPixelFormat(&mut format)?;
            target.WriteSource(&converter, std::ptr::null())?;
            target.Commit()?;
            encoder.Commit()?;
            Ok((width, height))
        }
    })();
    match result {
        Ok(size) => {
            std::fs::rename(&partial, destination)?;
            Ok(size)
        }
        Err(error) => {
            let _ = std::fs::remove_file(&partial);
            Err(error)
        }
    }
}
