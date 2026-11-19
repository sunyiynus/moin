use super::{CaptureFrame, MAX_CAPTURE_PIXELS, ScreenRect};
use anyhow::{Context, Result, bail, ensure};
use block::ConcreteBlock;
use cocoa::{
    base::{NO, YES, id, nil},
    foundation::{NSAutoreleasePool, NSPoint, NSRect, NSSize, NSString},
};
use objc::{class, msg_send, runtime::Class, sel, sel_impl};
use std::{
    ffi::{CStr, c_void},
    fs,
    os::unix::fs::DirBuilderExt,
    process::{Command, Stdio},
    sync::{
        OnceLock,
        atomic::{AtomicU64, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};

type CGPoint = NSPoint;
type CGSize = NSSize;
type CGRect = NSRect;

#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    fn CGGetActiveDisplayList(max_displays: u32, displays: *mut u32, count: *mut u32) -> i32;
    fn CGDisplayBounds(display: u32) -> CGRect;
    fn CGPreflightScreenCaptureAccess() -> bool;
    fn CGRequestScreenCaptureAccess() -> bool;
    fn CGImageGetWidth(image: *const c_void) -> usize;
    fn CGImageGetHeight(image: *const c_void) -> usize;
    fn CGColorSpaceCreateWithName(name: *const c_void) -> *mut c_void;
    fn CGColorSpaceRelease(space: *mut c_void);
    static kCGColorSpaceSRGB: *const c_void;
    fn CGBitmapContextCreate(
        data: *mut c_void,
        width: usize,
        height: usize,
        bits_per_component: usize,
        bytes_per_row: usize,
        space: *mut c_void,
        bitmap_info: u32,
    ) -> *mut c_void;
    fn CGContextDrawImage(context: *mut c_void, rect: CGRect, image: *const c_void);
    fn CGContextRelease(context: *mut c_void);
}

struct Pool(id);
impl Pool {
    fn new() -> Self {
        Self(unsafe { NSAutoreleasePool::new(nil) })
    }
}
impl Drop for Pool {
    fn drop(&mut self) {
        unsafe {
            let _: () = msg_send![self.0, drain];
        }
    }
}

struct OwnedObject(id);
impl Drop for OwnedObject {
    fn drop(&mut self) {
        unsafe {
            let _: () = msg_send![self.0, release];
        }
    }
}

struct CaptureObjects {
    filter: OwnedObject,
    configuration: OwnedObject,
    excludes_own_app: bool,
}
// These retained, private ScreenCaptureKit objects are configured fully on the
// completion queue and then transferred to one recording worker. They are never
// mutated concurrently or used as AppKit window/UI objects.
unsafe impl Send for CaptureObjects {}

pub struct CaptureSession {
    rect: ScreenRect,
    include_cursor: bool,
    objects: Option<CaptureObjects>,
}

fn screenshot_manager() -> Option<&'static Class> {
    static LOADED: OnceLock<bool> = OnceLock::new();
    let loaded = *LOADED.get_or_init(|| unsafe {
        // Loading dynamically keeps the older-system fallback available. Keep
        // this framework resident for the lifetime of its Objective-C classes.
        !libc::dlopen(
            c"/System/Library/Frameworks/ScreenCaptureKit.framework/ScreenCaptureKit".as_ptr(),
            libc::RTLD_LAZY | libc::RTLD_LOCAL,
        )
        .is_null()
    });
    loaded.then(|| Class::get("SCScreenshotManager")).flatten()
}

unsafe fn capture_error(error: id, fallback: &str) -> anyhow::Error {
    unsafe {
        if error == nil {
            return anyhow::anyhow!(fallback.to_owned());
        }
        let description: id = msg_send![error, localizedDescription];
        let bytes: *const std::ffi::c_char = msg_send![description, UTF8String];
        if bytes.is_null() {
            return anyhow::anyhow!(fallback.to_owned());
        }
        anyhow::anyhow!(
            "{fallback}：{}",
            CStr::from_ptr(bytes)
                .to_string_lossy()
                .chars()
                .take(400)
                .collect::<String>()
        )
    }
}

impl CaptureSession {
    pub fn new(rect: ScreenRect, include_cursor: bool) -> Result<Self> {
        rect.validate()?;
        let permitted =
            unsafe { CGPreflightScreenCaptureAccess() || CGRequestScreenCaptureAccess() };
        ensure!(
            permitted,
            "请在“系统设置 → 隐私与安全性 → 屏幕与系统音频录制”中允许 Moin 录屏，然后重新打开 Moin"
        );
        if screenshot_manager().is_none() {
            return Ok(Self {
                rect,
                include_cursor,
                objects: None,
            });
        }
        let _pool = Pool::new();
        let content_class =
            Class::get("SCShareableContent").context("系统缺少 ScreenCaptureKit 内容服务")?;
        let (sender, receiver) = mpsc::sync_channel(1);
        let block = ConcreteBlock::new(move |content: id, error: id| {
            let _pool = Pool::new();
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| unsafe {
                if error != nil || content == nil {
                    return Err(capture_error(
                        error,
                        "无法读取可捕获的显示器；请检查屏幕录制权限",
                    ));
                }
                make_capture_objects(content, rect, include_cursor)
            }))
            .unwrap_or_else(|_| Err(anyhow::anyhow!("ScreenCaptureKit 内容回调失败")));
            let _ = sender.send(result);
        })
        .copy();
        unsafe {
            let _: () = msg_send![content_class, getShareableContentExcludingDesktopWindows:NO onScreenWindowsOnly:NO completionHandler:&*block];
        }
        let objects = receiver
            .recv_timeout(Duration::from_secs(15))
            .context("读取 macOS 可截图内容超时")??;
        Ok(Self {
            rect,
            include_cursor,
            objects: Some(objects),
        })
    }

    pub fn require_own_windows_excluded(&self) -> Result<()> {
        let objects = self
            .objects
            .as_ref()
            .context("GIF 录制需要 macOS 14 或更新版本；当前系统可使用截图")?;
        ensure!(
            objects.excludes_own_app,
            "macOS 截图服务暂未识别 Moin 的录制窗口，无法排除控制栏；请重新开始录制"
        );
        Ok(())
    }

    pub fn capture(&mut self) -> Result<CaptureFrame> {
        let Some(objects) = &self.objects else {
            return system_capture(self.rect, self.include_cursor);
        };
        let _pool = Pool::new();
        let manager = screenshot_manager().context("系统缺少 ScreenCaptureKit 截图服务")?;
        let rect = self.rect;
        let (sender, receiver) = mpsc::sync_channel(1);
        let block = ConcreteBlock::new(move |image: *const c_void, error: id| {
            let _pool = Pool::new();
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| unsafe {
                if error != nil || image.is_null() {
                    return Err(capture_error(error, "macOS 截图失败"));
                }
                // Copy pixels before returning from this callback: ScreenCaptureKit
                // may recycle its image's backing storage immediately afterwards.
                rgba_from_cg_image(image, rect)
            }))
            .unwrap_or_else(|_| Err(anyhow::anyhow!("ScreenCaptureKit 图像回调失败")));
            let _ = sender.send(result);
        })
        .copy();
        unsafe {
            let _: () = msg_send![manager, captureImageWithFilter:objects.filter.0 configuration:objects.configuration.0 completionHandler:&*block];
        }
        receiver
            .recv_timeout(Duration::from_secs(15))
            .context("macOS 截图超时，请检查屏幕录制权限")?
    }
}

unsafe fn make_capture_objects(
    content: id,
    rect: ScreenRect,
    include_cursor: bool,
) -> Result<CaptureObjects> {
    unsafe {
        let displays: id = msg_send![content, displays];
        let count: usize = msg_send![displays, count];
        let mut selected = nil;
        let mut selected_bounds = NSRect::new(NSPoint::new(0., 0.), NSSize::new(0., 0.));
        for index in 0..count {
            let display: id = msg_send![displays, objectAtIndex:index];
            let bounds: NSRect = msg_send![display, frame];
            if rect.x as f64 >= bounds.origin.x
                && rect.y as f64 >= bounds.origin.y
                && rect.x as f64 + rect.width as f64 <= bounds.origin.x + bounds.size.width
                && rect.y as f64 + rect.height as f64 <= bounds.origin.y + bounds.size.height
            {
                selected = display;
                selected_bounds = bounds;
                break;
            }
        }
        ensure!(selected != nil, "所选区域不在同一显示器内，请重新选择区域");
        let applications: id = msg_send![content, applications];
        let count: usize = msg_send![applications, count];
        let excluded: id = msg_send![class!(NSMutableArray), array];
        let mut excludes_own_app = false;
        for index in 0..count {
            let application: id = msg_send![applications, objectAtIndex:index];
            let pid: i32 = msg_send![application, processID];
            if pid == std::process::id() as i32 {
                let _: () = msg_send![excluded, addObject:application];
                excludes_own_app = true;
            }
        }
        let exceptions: id = msg_send![class!(NSArray), array];
        let filter: id = msg_send![class!(SCContentFilter), alloc];
        let filter = OwnedObject(
            msg_send![filter, initWithDisplay:selected excludingApplications:excluded exceptingWindows:exceptions],
        );
        ensure!(filter.0 != nil, "无法创建 macOS 截图内容过滤器");
        let scale: f32 = msg_send![filter.0, pointPixelScale];
        ensure!(
            scale.is_finite() && scale > 0.,
            "显示器的 Retina 缩放比例无效"
        );
        let width = (rect.width as f64 * scale as f64).round().max(1.) as usize;
        let height = (rect.height as f64 * scale as f64).round().max(1.) as usize;
        ensure!(
            width as u64 * height as u64 <= MAX_CAPTURE_PIXELS,
            "截图超过 4000 万像素，请缩小选区"
        );
        let configuration: id = msg_send![class!(SCStreamConfiguration), new];
        let configuration = OwnedObject(configuration);
        ensure!(configuration.0 != nil, "无法创建 macOS 截图配置");
        let source = NSRect::new(
            NSPoint::new(
                rect.x as f64 - selected_bounds.origin.x,
                rect.y as f64 - selected_bounds.origin.y,
            ),
            NSSize::new(rect.width as f64, rect.height as f64),
        );
        let _: () = msg_send![configuration.0, setSourceRect:source];
        let _: () = msg_send![configuration.0, setWidth:width];
        let _: () = msg_send![configuration.0, setHeight:height];
        let _: () = msg_send![configuration.0, setScalesToFit:NO];
        let _: () =
            msg_send![configuration.0, setShowsCursor:if include_cursor { YES } else { NO }];
        Ok(CaptureObjects {
            filter,
            configuration,
            excludes_own_app,
        })
    }
}

unsafe fn rgba_from_cg_image(image: *const c_void, rect: ScreenRect) -> Result<CaptureFrame> {
    unsafe {
        let width = CGImageGetWidth(image);
        let height = CGImageGetHeight(image);
        ensure!(
            width > 0 && height > 0 && width as u64 * height as u64 <= MAX_CAPTURE_PIXELS,
            "macOS 截图图像尺寸无效或过大"
        );
        let mut rgba = vec![0_u8; width * height * 4];
        let space = CGColorSpaceCreateWithName(kCGColorSpaceSRGB);
        ensure!(!space.is_null(), "无法创建 sRGB 颜色空间");
        // Explicit big-endian RGBA, premultiplied last; do not depend on the
        // source buffer being BGRA8 (HDR displays can supply a different format).
        let context = CGBitmapContextCreate(
            rgba.as_mut_ptr().cast(),
            width,
            height,
            8,
            width * 4,
            space,
            0x4001,
        );
        CGColorSpaceRelease(space);
        ensure!(!context.is_null(), "无法转换 macOS 截图像素");
        CGContextDrawImage(
            context,
            NSRect::new(
                NSPoint::new(0., 0.),
                NSSize::new(width as f64, height as f64),
            ),
            image,
        );
        CGContextRelease(context);
        for pixel in rgba.chunks_exact_mut(4) {
            let alpha = pixel[3] as u32;
            if alpha > 0 && alpha < 255 {
                for channel in &mut pixel[..3] {
                    *channel = ((*channel as u32 * 255 + alpha / 2) / alpha).min(255) as u8;
                }
            }
        }
        Ok(CaptureFrame {
            rect,
            width: width as u32,
            height: height as u32,
            rgba,
        })
    }
}

pub fn monitors() -> Result<Vec<ScreenRect>> {
    let mut displays = [0_u32; 32];
    let mut count = 0;
    let status =
        unsafe { CGGetActiveDisplayList(displays.len() as u32, displays.as_mut_ptr(), &mut count) };
    ensure!(status == 0, "无法读取显示器列表（macOS 错误 {status}）");
    let mut screens = Vec::new();
    for display in displays
        .iter()
        .take(count.min(displays.len() as u32) as usize)
    {
        let bounds = unsafe { CGDisplayBounds(*display) };
        let rect = ScreenRect {
            x: bounds.origin.x.round() as i32,
            y: bounds.origin.y.round() as i32,
            width: bounds.size.width.round().max(0.0) as u32,
            height: bounds.size.height.round().max(0.0) as u32,
        };
        if rect.width > 0 && rect.height > 0 && !screens.contains(&rect) {
            screens.push(rect);
        }
    }
    ensure!(!screens.is_empty(), "未找到可截图的显示器");
    Ok(screens)
}

static NEXT_CAPTURE: AtomicU64 = AtomicU64::new(1);
struct TemporaryDirectory(std::path::PathBuf);
impl Drop for TemporaryDirectory {
    fn drop(&mut self) {
        // Only the two fixed files created in our private, create_dir directory.
        let _ = fs::remove_file(self.0.join("frame.png"));
        let _ = fs::remove_file(self.0.join("error.txt"));
        let _ = fs::remove_dir(&self.0);
    }
}

pub fn capture_rect(rect: ScreenRect, include_cursor: bool) -> Result<CaptureFrame> {
    CaptureSession::new(rect, include_cursor)?.capture()
}

fn system_capture(rect: ScreenRect, include_cursor: bool) -> Result<CaptureFrame> {
    rect.validate()?;
    // Use the system's modern capture implementation, rather than removed
    // CGDisplayCreateImage/CGWindowListCreateImage APIs on newer macOS releases.
    let permitted = unsafe { CGPreflightScreenCaptureAccess() || CGRequestScreenCaptureAccess() };
    ensure!(
        permitted,
        "请在“系统设置 → 隐私与安全性 → 屏幕与系统音频录制”中允许 Moin 录屏，然后重新打开 Moin"
    );
    let directory = loop {
        let path = std::env::temp_dir().join(format!(
            "moin-capture-{}-{}",
            std::process::id(),
            NEXT_CAPTURE.fetch_add(1, Ordering::Relaxed)
        ));
        match fs::DirBuilder::new().mode(0o700).create(&path) {
            Ok(()) => break TemporaryDirectory(path),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error).context("无法创建截图临时目录"),
        }
    };
    let path = directory.0.join("frame.png");
    let error_file = fs::File::create(directory.0.join("error.txt"))?;
    let mut command = Command::new("/usr/sbin/screencapture");
    command.arg("-x").arg("-t").arg("png");
    if include_cursor {
        command.arg("-C");
    }
    command
        .arg(format!(
            "-R{},{},{},{}",
            rect.x, rect.y, rect.width, rect.height
        ))
        .arg(&path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::from(error_file));
    let mut child = command.spawn().context("无法启动 macOS 系统截图服务")?;
    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {}
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(error).context("无法等待 macOS 截图进程");
            }
        }
        if started.elapsed() >= Duration::from_secs(15) {
            let _ = child.kill();
            let _ = child.wait();
            bail!("macOS 截图超时，请检查屏幕录制权限");
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    if !status.success() {
        let error = fs::read_to_string(directory.0.join("error.txt")).unwrap_or_default();
        bail!(
            "macOS 截图失败；请检查 Moin 的屏幕录制权限。{}",
            error.chars().take(300).collect::<String>().trim()
        );
    }
    ensure!(
        fs::metadata(&path)?.len() <= MAX_CAPTURE_PIXELS * 4,
        "截图文件过大，请缩小选区"
    );
    let (width, height) = image::image_dimensions(&path)?;
    ensure!(
        width > 0 && height > 0 && width as u64 * height as u64 <= MAX_CAPTURE_PIXELS,
        "截图超过 4000 万像素，请缩小选区"
    );
    let rgba = image::open(&path)
        .context("无法读取 macOS 截图")?
        .into_rgba8()
        .into_raw();
    Ok(CaptureFrame {
        rect,
        width,
        height,
        rgba,
    })
}

pub fn copy_image(frame: &CaptureFrame) -> Result<()> {
    let bytes = super::png_bytes(frame)?;
    unsafe {
        let pool = NSAutoreleasePool::new(nil);
        let result = (|| -> Result<()> {
            let data: id =
                msg_send![class!(NSData), dataWithBytes: bytes.as_ptr() length: bytes.len()];
            ensure!(data != nil, "无法创建剪贴板图像");
            let pasteboard: id = msg_send![class!(NSPasteboard), generalPasteboard];
            ensure!(pasteboard != nil, "无法访问系统剪贴板");
            let format = NSString::alloc(nil).init_str("public.png");
            let _: isize = msg_send![pasteboard, clearContents];
            let success: cocoa::base::BOOL = msg_send![pasteboard, setData: data forType: format];
            let _: () = msg_send![format, release];
            ensure!(success == cocoa::base::YES, "无法复制截图到剪贴板");
            Ok(())
        })();
        let _: () = msg_send![pool, drain];
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[link(name = "CoreGraphics", kind = "framework")]
    unsafe extern "C" {
        fn CGBitmapContextCreateImage(context: *mut c_void) -> *mut c_void;
        fn CGImageRelease(image: *mut c_void);
    }

    #[test]
    fn offscreen_coregraphics_conversion_preserves_rgba_and_row_order() {
        // No display capture, permission request, window, or pasteboard access.
        unsafe {
            let mut source = vec![
                255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 50, 100, 150, 255,
            ];
            let space = CGColorSpaceCreateWithName(kCGColorSpaceSRGB);
            assert!(!space.is_null());
            let context =
                CGBitmapContextCreate(source.as_mut_ptr().cast(), 2, 2, 8, 8, space, 0x4001);
            CGColorSpaceRelease(space);
            assert!(!context.is_null());
            let image = CGBitmapContextCreateImage(context);
            assert!(!image.is_null());
            let result = rgba_from_cg_image(
                image,
                ScreenRect {
                    x: -100,
                    y: 0,
                    width: 1,
                    height: 1,
                },
            );
            CGImageRelease(image);
            CGContextRelease(context);
            let result = result.unwrap();
            assert_eq!((result.width, result.height), (2, 2));
            assert_eq!(result.rgba, source);
        }
    }
}
