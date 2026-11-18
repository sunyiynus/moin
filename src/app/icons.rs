use gpui::{App, RenderImage, Window};
use std::{path::Path, sync::Arc};

pub fn app_icon(path: &Path, window: &mut Window, cx: &mut App) -> Option<Arc<RenderImage>> {
    #[cfg(windows)]
    {
        return windows_icons::get(windows_icons::Source::App(path.to_owned()), window, cx);
    }
    #[cfg(not(windows))]
    {
        let _ = (path, window, cx);
        None
    }
}

pub fn file_icon(path: &Path, window: &mut Window, cx: &mut App) -> Option<Arc<RenderImage>> {
    #[cfg(windows)]
    {
        let extension = path
            .extension()
            .and_then(|extension| extension.to_str())
            .filter(|extension| extension.len() <= 64)
            .unwrap_or_default()
            .to_ascii_lowercase();
        return windows_icons::get(windows_icons::Source::FileType(extension), window, cx);
    }
    #[cfg(not(windows))]
    {
        let _ = (path, window, cx);
        None
    }
}

#[cfg(windows)]
mod windows_icons {
    use super::{App, Arc, RenderImage, Window};
    use gpui::Asset;
    use image::{Frame, RgbaImage};
    use std::{
        future::Future, mem::size_of, os::windows::ffi::OsStrExt, path::PathBuf, ptr, slice,
    };
    use windows::{
        Win32::{
            Foundation::RPC_E_CHANGED_MODE,
            Graphics::Gdi::{
                BI_RGB, BITMAPINFO, BITMAPINFOHEADER, CreateCompatibleDC, CreateDIBSection,
                DIB_RGB_COLORS, DeleteDC, DeleteObject, GdiFlush, HBITMAP, HDC, HGDIOBJ,
                SelectObject,
            },
            Storage::FileSystem::FILE_ATTRIBUTE_NORMAL,
            System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx, CoUninitialize},
            UI::{
                Shell::{
                    SHFILEINFOW, SHGFI_ICON, SHGFI_LARGEICON, SHGFI_USEFILEATTRIBUTES,
                    SHGetFileInfoW,
                },
                WindowsAndMessaging::{DI_NORMAL, DestroyIcon, DrawIconEx, HICON},
            },
        },
        core::HSTRING,
    };

    const ICON_SIZE: u32 = 32;
    const SURFACE_BYTES: usize = (ICON_SIZE * ICON_SIZE * 4) as usize;

    #[derive(Clone, Hash)]
    pub enum Source {
        App(PathBuf),
        FileType(String),
    }

    struct ShellIcon;

    impl Asset for ShellIcon {
        type Source = Source;
        type Output = Option<Arc<RenderImage>>;

        fn load(
            source: Source,
            _cx: &mut App,
        ) -> impl Future<Output = Self::Output> + Send + 'static {
            async move { load(source) }
        }
    }

    pub fn get(source: Source, window: &mut Window, cx: &mut App) -> Option<Arc<RenderImage>> {
        let (asset, first) = cx.fetch_asset::<ShellIcon>(&source);
        if let Some(icon) = asset.peek() {
            return icon.clone();
        }
        if first {
            window
                .spawn(cx, async move |cx| {
                    asset.await;
                    let _ = cx.update(|window, _| window.refresh());
                })
                .detach();
        }
        None
    }

    struct ComApartment(bool);

    impl ComApartment {
        fn enter() -> Option<Self> {
            let result = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
            if result.is_ok() {
                Some(Self(true))
            } else if result == RPC_E_CHANGED_MODE {
                // GPUI may reuse a worker which already entered another COM apartment.
                Some(Self(false))
            } else {
                None
            }
        }
    }

    impl Drop for ComApartment {
        fn drop(&mut self) {
            if self.0 {
                unsafe { CoUninitialize() };
            }
        }
    }

    struct OwnedIcon(HICON);

    impl Drop for OwnedIcon {
        fn drop(&mut self) {
            if !self.0.is_invalid() {
                let _ = unsafe { DestroyIcon(self.0) };
            }
        }
    }

    fn load(source: Source) -> Option<Arc<RenderImage>> {
        let _apartment = ComApartment::enter()?;
        let (name, attribute_only) = match source {
            Source::App(path) => {
                // Start Menu roots can contain mixed separators. Shell icon lookup
                // for .lnk files expects Windows separators even on accessible paths.
                let wide: Vec<_> = path
                    .as_os_str()
                    .encode_wide()
                    .map(|unit| {
                        if unit == u16::from(b'/') {
                            u16::from(b'\\')
                        } else {
                            unit
                        }
                    })
                    .collect();
                (HSTRING::from_wide(&wide), false)
            }
            Source::FileType(extension) if extension.is_empty() => (HSTRING::from("file"), true),
            Source::FileType(extension) => (HSTRING::from(format!("file.{extension}")), true),
        };
        let flags = if attribute_only {
            SHGFI_ICON | SHGFI_LARGEICON | SHGFI_USEFILEATTRIBUTES
        } else {
            SHGFI_ICON | SHGFI_LARGEICON
        };
        let mut info = SHFILEINFOW::default();
        let count = unsafe {
            SHGetFileInfoW(
                &name,
                FILE_ATTRIBUTE_NORMAL,
                Some(&mut info),
                size_of::<SHFILEINFOW>() as u32,
                flags,
            )
        };
        let icon = OwnedIcon(info.hIcon);
        if count == 0 || icon.0.is_invalid() {
            return None;
        }
        let mut surface = Surface::new()?;
        let mut bgra = surface.draw(icon.0, 0)?.to_vec();
        let on_white = surface.draw(icon.0, 255)?;
        restore_alpha(&mut bgra, on_white);
        let buffer = RgbaImage::from_raw(ICON_SIZE, ICON_SIZE, bgra)?;
        // RenderImage stores BGRA even though image::Frame calls its buffer RgbaImage.
        Some(Arc::new(RenderImage::new([Frame::new(buffer)])))
    }

    struct Surface {
        dc: HDC,
        bitmap: HBITMAP,
        original: HGDIOBJ,
        bits: *mut u8,
    }

    impl Surface {
        fn new() -> Option<Self> {
            let info = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: ICON_SIZE as i32,
                    biHeight: -(ICON_SIZE as i32),
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB.0,
                    ..Default::default()
                },
                ..Default::default()
            };
            unsafe {
                let dc = CreateCompatibleDC(None);
                if dc.is_invalid() {
                    return None;
                }
                let mut bits = ptr::null_mut();
                let Ok(bitmap) =
                    CreateDIBSection(Some(dc), &info, DIB_RGB_COLORS, &mut bits, None, 0)
                else {
                    let _ = DeleteDC(dc);
                    return None;
                };
                let original = SelectObject(dc, bitmap.into());
                if original.is_invalid() || bits.is_null() {
                    if !original.is_invalid() {
                        SelectObject(dc, original);
                    }
                    let _ = DeleteObject(bitmap.into());
                    let _ = DeleteDC(dc);
                    return None;
                }
                Some(Self {
                    dc,
                    bitmap,
                    original,
                    bits: bits.cast(),
                })
            }
        }

        fn draw(&mut self, icon: HICON, background: u8) -> Option<&[u8]> {
            unsafe {
                for pixel in slice::from_raw_parts_mut(self.bits, SURFACE_BYTES).chunks_exact_mut(4)
                {
                    pixel.copy_from_slice(&[background, background, background, 255]);
                }
                DrawIconEx(
                    self.dc,
                    0,
                    0,
                    icon,
                    ICON_SIZE as i32,
                    ICON_SIZE as i32,
                    0,
                    None,
                    DI_NORMAL,
                )
                .ok()?;
                if !GdiFlush().as_bool() {
                    return None;
                }
                Some(slice::from_raw_parts(self.bits, SURFACE_BYTES))
            }
        }
    }

    impl Drop for Surface {
        fn drop(&mut self) {
            unsafe {
                SelectObject(self.dc, self.original);
                let _ = DeleteObject(self.bitmap.into());
                let _ = DeleteDC(self.dc);
            }
        }
    }

    // Reconstruct straight-alpha BGRA from the black and white composites.
    // DrawIconEx handles both alpha icons and earlier icons with AND/XOR masks.
    fn restore_alpha(on_black: &mut [u8], on_white: &[u8]) {
        for (dark, light) in on_black.chunks_exact_mut(4).zip(on_white.chunks_exact(4)) {
            let uncovered = light[0]
                .saturating_sub(dark[0])
                .max(light[1].saturating_sub(dark[1]))
                .max(light[2].saturating_sub(dark[2]));
            let opacity = 255 - uncovered;
            if opacity == 0 {
                dark.fill(0);
            } else {
                let alpha = u32::from(opacity);
                for channel in &mut dark[..3] {
                    *channel = ((u32::from(*channel) * 255 + alpha / 2) / alpha).min(255) as u8;
                }
                dark[3] = opacity;
            }
        }
    }
}
