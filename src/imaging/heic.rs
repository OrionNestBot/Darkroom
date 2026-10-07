//! HEIC/HEIF decoding via Windows Imaging Component (WIC); no external DLL needed.
//! Requires the "HEIF Image Extensions" package from the Microsoft Store.

use anyhow::{Context, Result, anyhow};
use std::path::Path;
use windows::Win32::Foundation::GENERIC_READ;
use windows::Win32::Graphics::Imaging::*;
use windows::Win32::System::Com::*;
use windows::core::HSTRING;

fn com_init() {
    // Once per thread; an existing initialization in another mode can be ignored.
    thread_local! {
        static INIT: () = unsafe { let _ = CoInitializeEx(None, COINIT_MULTITHREADED); };
    }
    INIT.with(|_| {});
}

/// RGBA8 decoding. With `max_edge`, the WIC scaler downsizes during decoding (fast).
pub fn decode_rgba(path: &Path, max_edge: Option<u32>) -> Result<(Vec<u8>, u32, u32)> {
    com_init();
    unsafe {
        let factory: IWICImagingFactory =
            CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER).context(tr!("WIC 팩토리 생성 실패", "Couldn't create WIC factory"))?;
        let wpath = HSTRING::from(path.as_os_str());
        let decoder = factory
            .CreateDecoderFromFilename(&wpath, None, GENERIC_READ, WICDecodeMetadataCacheOnDemand)
            .map_err(|e| anyhow!("{}", trf!("HEIC 디코더를 열 수 없음 (HEIF 이미지 확장 설치 필요): {e}", "Can't open HEIC decoder (install HEIF Image Extensions): {e}")))?;
        let frame = decoder.GetFrame(0)?;
        let (mut w, mut h) = (0u32, 0u32);
        frame.GetSize(&mut w, &mut h)?;
        let source: IWICBitmapSource = match max_edge {
            Some(m) if w.max(h) > m => {
                let k = m as f32 / w.max(h) as f32;
                let (nw, nh) = (((w as f32 * k).round() as u32).max(1), ((h as f32 * k).round() as u32).max(1));
                let scaler = factory.CreateBitmapScaler()?;
                scaler.Initialize(&frame, nw, nh, WICBitmapInterpolationModeFant)?;
                w = nw;
                h = nh;
                scaler.into()
            }
            _ => frame.into(),
        };
        let conv = factory.CreateFormatConverter()?;
        conv.Initialize(&source, &GUID_WICPixelFormat32bppRGBA, WICBitmapDitherTypeNone, None, 0.0, WICBitmapPaletteTypeCustom)?;
        let stride = w * 4;
        let mut buf = vec![0u8; (stride * h) as usize];
        conv.CopyPixels(std::ptr::null(), stride, &mut buf)?;
        Ok((buf, w, h))
    }
}
