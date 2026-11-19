use super::{CaptureFrame, ScreenRect};
use anyhow::{Context, Result, ensure};
use std::{mem::size_of, ptr, time::Duration};
use windows::{
    Win32::{
        Foundation::{GlobalFree, HANDLE, HGLOBAL, HWND, LPARAM, RECT},
        Graphics::Gdi::{
            BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BitBlt, CAPTUREBLT, CreateCompatibleDC,
            CreateDIBSection, DIB_RGB_COLORS, DeleteDC, DeleteObject, EnumDisplayMonitors,
            GdiFlush, GetDC, HBITMAP, HDC, HGDIOBJ, HMONITOR, ReleaseDC, SRCCOPY, SelectObject,
        },
        System::{
            DataExchange::{
                CloseClipboard, EmptyClipboard, OpenClipboard, RegisterClipboardFormatW,
                SetClipboardData,
            },
            Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalUnlock},
        },
        UI::{
            HiDpi::{
                DPI_AWARENESS_CONTEXT, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
                SetThreadDpiAwarenessContext,
            },
            WindowsAndMessaging::{
                CURSOR_SHOWING, CURSORINFO, CreateWindowExW, DI_NORMAL, DestroyWindow, DrawIconEx,
                GetCursorInfo, GetIconInfo, HICON, HWND_MESSAGE, ICONINFO, WINDOW_EX_STYLE,
                WS_POPUP,
            },
        },
    },
    core::{BOOL, w},
};

struct PhysicalPixelScope(DPI_AWARENESS_CONTEXT);
impl PhysicalPixelScope {
    fn new() -> Result<Self> {
        let previous =
            unsafe { SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
        // -1 is a valid previous context (DPI_UNAWARE), so only NULL means failure.
        ensure!(
            !previous.0.is_null(),
            "无法切换截图线程的 DPI 上下文：{}",
            windows::core::Error::from_win32()
        );
        Ok(Self(previous))
    }
}
impl Drop for PhysicalPixelScope {
    fn drop(&mut self) {
        unsafe {
            SetThreadDpiAwarenessContext(self.0);
        }
    }
}

pub fn monitors() -> Result<Vec<ScreenRect>> {
    let _physical_pixels = PhysicalPixelScope::new()?;
    unsafe extern "system" fn collect(_: HMONITOR, _: HDC, rect: *mut RECT, data: LPARAM) -> BOOL {
        if !rect.is_null() {
            let rect = unsafe { *rect };
            let screens = unsafe { &mut *(data.0 as *mut Vec<ScreenRect>) };
            if rect.right > rect.left && rect.bottom > rect.top {
                screens.push(ScreenRect {
                    x: rect.left,
                    y: rect.top,
                    width: (rect.right - rect.left) as u32,
                    height: (rect.bottom - rect.top) as u32,
                });
            }
        }
        BOOL(1)
    }
    let mut screens = Vec::new();
    unsafe {
        EnumDisplayMonitors(
            None,
            None,
            Some(collect),
            LPARAM((&mut screens as *mut Vec<ScreenRect>) as isize),
        )
        .ok()?;
    }
    ensure!(!screens.is_empty(), "未找到可截图的显示器");
    Ok(screens)
}

struct DesktopDc(HDC);
impl Drop for DesktopDc {
    fn drop(&mut self) {
        unsafe {
            let _ = ReleaseDC(None, self.0);
        }
    }
}
struct MemoryDc(HDC);
impl Drop for MemoryDc {
    fn drop(&mut self) {
        unsafe {
            let _ = DeleteDC(self.0);
        }
    }
}
struct Bitmap(HBITMAP);
impl Drop for Bitmap {
    fn drop(&mut self) {
        unsafe {
            let _ = DeleteObject(self.0.into());
        }
    }
}
struct SelectedObject(HDC, HGDIOBJ);
impl Drop for SelectedObject {
    fn drop(&mut self) {
        unsafe {
            SelectObject(self.0, self.1);
        }
    }
}

pub fn capture_rect(rect: ScreenRect, include_cursor: bool) -> Result<CaptureFrame> {
    rect.validate()?;
    let _physical_pixels = PhysicalPixelScope::new()?;
    unsafe {
        let source = DesktopDc(GetDC(None));
        ensure!(!source.0.is_invalid(), "无法访问桌面图像");
        copy_surface(source.0, rect, include_cursor)
    }
}

fn copy_surface(source: HDC, rect: ScreenRect, include_cursor: bool) -> Result<CaptureFrame> {
    unsafe {
        let target = MemoryDc(CreateCompatibleDC(Some(source)));
        ensure!(!target.0.is_invalid(), "无法创建截图画布");
        let info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: rect.width as i32,
                biHeight: -(rect.height as i32),
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut pixels = ptr::null_mut();
        let bitmap = Bitmap(
            CreateDIBSection(Some(source), &info, DIB_RGB_COLORS, &mut pixels, None, 0)
                .context("无法分配截图内存")?,
        );
        ensure!(!pixels.is_null(), "截图像素分配失败");
        let previous = SelectObject(target.0, bitmap.0.into());
        ensure!(
            !previous.is_invalid() && previous.0 as isize != -1,
            "无法选择截图画布"
        );
        let _selected = SelectedObject(target.0, previous);
        BitBlt(
            target.0,
            0,
            0,
            rect.width as i32,
            rect.height as i32,
            Some(source),
            rect.x,
            rect.y,
            SRCCOPY | CAPTUREBLT,
        )
        .context("桌面截图失败")?;
        if include_cursor {
            draw_cursor(target.0, rect);
        }
        GdiFlush().ok().context("无法完成桌面截图")?;
        let bytes = std::slice::from_raw_parts(
            pixels.cast::<u8>(),
            rect.width as usize * rect.height as usize * 4,
        );
        let mut rgba = bytes.to_vec();
        for pixel in rgba.chunks_exact_mut(4) {
            pixel.swap(0, 2);
            pixel[3] = 255;
        }
        Ok(CaptureFrame {
            rect,
            width: rect.width,
            height: rect.height,
            rgba,
        })
    }
}

unsafe fn draw_cursor(target: HDC, rect: ScreenRect) {
    unsafe {
        let mut cursor = CURSORINFO {
            cbSize: size_of::<CURSORINFO>() as u32,
            ..Default::default()
        };
        if GetCursorInfo(&mut cursor).is_err() || cursor.flags != CURSOR_SHOWING {
            return;
        }
        let icon = HICON(cursor.hCursor.0);
        let mut info = ICONINFO::default();
        if GetIconInfo(icon, &mut info).is_err() {
            return;
        }
        let _mask = Bitmap(info.hbmMask);
        let _color = Bitmap(info.hbmColor);
        let x = cursor.ptScreenPos.x - rect.x - info.xHotspot as i32;
        let y = cursor.ptScreenPos.y - rect.y - info.yHotspot as i32;
        let _ = DrawIconEx(target, x, y, icon, 0, 0, 0, None, DI_NORMAL);
    }
}

struct ClipboardOwner(HWND);
impl Drop for ClipboardOwner {
    fn drop(&mut self) {
        unsafe {
            let _ = DestroyWindow(self.0);
        }
    }
}
struct OpenClipboardGuard;
impl Drop for OpenClipboardGuard {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseClipboard();
        }
    }
}
struct ClipboardMemory(HGLOBAL);
impl Drop for ClipboardMemory {
    fn drop(&mut self) {
        unsafe {
            let _ = GlobalFree(Some(self.0));
        }
    }
}

unsafe fn clipboard_bytes(bytes: &[u8]) -> Result<ClipboardMemory> {
    unsafe {
        let memory = ClipboardMemory(GlobalAlloc(GMEM_MOVEABLE, bytes.len())?);
        let destination = GlobalLock(memory.0);
        ensure!(!destination.is_null(), "无法分配剪贴板图像内存");
        ptr::copy_nonoverlapping(bytes.as_ptr(), destination.cast::<u8>(), bytes.len());
        let _ = GlobalUnlock(memory.0);
        Ok(memory)
    }
}

pub fn copy_image(frame: &CaptureFrame) -> Result<()> {
    frame.validate()?;
    let png = super::png_bytes(frame)?;
    let header = BITMAPINFOHEADER {
        biSize: size_of::<BITMAPINFOHEADER>() as u32,
        biWidth: frame.width as i32,
        biHeight: -(frame.height as i32),
        biPlanes: 1,
        biBitCount: 32,
        biCompression: BI_RGB.0,
        biSizeImage: frame.rgba.len() as u32,
        ..Default::default()
    };
    let mut dib = Vec::with_capacity(size_of::<BITMAPINFOHEADER>() + frame.rgba.len());
    unsafe {
        dib.extend_from_slice(std::slice::from_raw_parts(
            (&raw const header).cast::<u8>(),
            size_of::<BITMAPINFOHEADER>(),
        ));
    }
    for pixel in frame.rgba.chunks_exact(4) {
        dib.extend_from_slice(&[pixel[2], pixel[1], pixel[0], 255]);
    }
    unsafe {
        // A message-only owned window gives SetClipboardData a valid owner, without
        // creating or activating any visible window on the user's desktop.
        let owner = ClipboardOwner(CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            w!("STATIC"),
            w!("Moin clipboard"),
            WS_POPUP,
            0,
            0,
            0,
            0,
            Some(HWND_MESSAGE),
            None,
            None,
            None,
        )?);
        let mut opened = false;
        for _ in 0..10 {
            if OpenClipboard(Some(owner.0)).is_ok() {
                opened = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(15));
        }
        ensure!(opened, "剪贴板正被其他程序占用，请重试");
        let _clipboard = OpenClipboardGuard;
        let dib_memory = clipboard_bytes(&dib)?;
        let png_memory = clipboard_bytes(&png)?;
        let png_format = RegisterClipboardFormatW(w!("PNG"));
        ensure!(png_format != 0, "无法注册 PNG 剪贴板格式");
        EmptyClipboard()?;
        SetClipboardData(8, Some(HANDLE(dib_memory.0.0)))?; // CF_DIB
        std::mem::forget(dib_memory); // The operating system now owns the memory.
        if SetClipboardData(png_format, Some(HANDLE(png_memory.0.0))).is_ok() {
            std::mem::forget(png_memory);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn offscreen_gdi_capture_preserves_rows_colors_and_crop() {
        // Exercise the native BitBlt/DIB pipeline against an in-memory surface.
        // This test does not access the user's screen or change the clipboard.
        unsafe {
            let source = MemoryDc(CreateCompatibleDC(None));
            assert!(!source.0.is_invalid());
            let info = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: 4,
                    biHeight: -3,
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB.0,
                    ..Default::default()
                },
                ..Default::default()
            };
            let mut pixels = ptr::null_mut();
            let bitmap = Bitmap(
                CreateDIBSection(None, &info, DIB_RGB_COLORS, &mut pixels, None, 0).unwrap(),
            );
            let _selected = SelectedObject(source.0, SelectObject(source.0, bitmap.0.into()));
            let bytes = std::slice::from_raw_parts_mut(pixels.cast::<u8>(), 4 * 3 * 4);
            for (index, pixel) in bytes.chunks_exact_mut(4).enumerate() {
                pixel.copy_from_slice(&[index as u8, 50, 200, 0]);
            }
            let captured = copy_surface(
                source.0,
                ScreenRect {
                    x: 1,
                    y: 1,
                    width: 2,
                    height: 2,
                },
                false,
            )
            .unwrap();
            assert_eq!(
                captured.rgba,
                [
                    200, 50, 5, 255, 200, 50, 6, 255, 200, 50, 9, 255, 200, 50, 10, 255
                ]
            );
        }
    }
}
