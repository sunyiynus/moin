//! Desktop capture and bounded, streaming GIF encoding. No capture starts until invoked.
use anyhow::{Context, Result, bail, ensure};
use image::{ImageEncoder, RgbaImage};
use std::{
    fs::{self, File, OpenOptions},
    io::{self, BufWriter, Write},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

#[cfg(windows)]
#[path = "capture/windows.rs"]
mod native;
#[cfg(target_os = "macos")]
#[path = "capture/macos.rs"]
mod native;

pub const MAX_CAPTURE_PIXELS: u64 = 40_000_000;
const MAX_GIF_PIXELS: u64 = 2_073_600;
const MAX_GIF_EDGE: u32 = 1920;
const MAX_GIF_BYTES: u64 = 256 * 1024 * 1024;
static NEXT_FILE: AtomicU64 = AtomicU64::new(1);

/// Global desktop coordinates: physical pixels on Windows, points on macOS.
/// macOS Retina pixel dimensions are retained separately in `CaptureFrame`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ScreenRect {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

impl ScreenRect {
    pub fn validate(self) -> Result<()> {
        ensure!(self.width > 0 && self.height > 0, "截图区域不能为空");
        ensure!(
            self.width <= i32::MAX as u32 && self.height <= i32::MAX as u32,
            "截图区域过大"
        );
        ensure!(
            self.width as u64 * self.height as u64 <= MAX_CAPTURE_PIXELS,
            "截图区域超过 4000 万像素，请缩小选区"
        );
        ensure!(
            (self.x as i64 + self.width as i64) <= i32::MAX as i64
                && (self.y as i64 + self.height as i64) <= i32::MAX as i64,
            "截图区域坐标无效"
        );
        Ok(())
    }
}

#[derive(Clone, Debug)]
pub struct CaptureFrame {
    pub rect: ScreenRect,
    pub width: u32,
    pub height: u32,
    /// Top-to-bottom, unpremultiplied RGBA8 pixels.
    pub rgba: Vec<u8>,
}

impl CaptureFrame {
    pub fn validate(&self) -> Result<()> {
        self.rect.validate()?;
        ensure!(self.width > 0 && self.height > 0, "截图像素不能为空");
        let pixels = self.width as u64 * self.height as u64;
        ensure!(
            pixels <= MAX_CAPTURE_PIXELS && pixels * 4 == self.rgba.len() as u64,
            "截图像素尺寸无效或过大"
        );
        Ok(())
    }

    /// Crop in image pixels; retain the corresponding global desktop rectangle.
    pub fn crop(&self, x: u32, y: u32, width: u32, height: u32) -> Result<Self> {
        self.validate()?;
        ensure!(
            width > 0
                && height > 0
                && x as u64 + width as u64 <= self.width as u64
                && y as u64 + height as u64 <= self.height as u64,
            "截图选区超出图像范围"
        );
        let mut rgba = Vec::with_capacity(width as usize * height as usize * 4);
        for row in y..y + height {
            let start = ((row as usize * self.width as usize) + x as usize) * 4;
            rgba.extend_from_slice(&self.rgba[start..start + width as usize * 4]);
        }
        let left = (x as f64 * self.rect.width as f64 / self.width as f64).round() as i64;
        let top = (y as f64 * self.rect.height as f64 / self.height as f64).round() as i64;
        let right =
            ((x + width) as f64 * self.rect.width as f64 / self.width as f64).round() as i64;
        let bottom =
            ((y + height) as f64 * self.rect.height as f64 / self.height as f64).round() as i64;
        Ok(Self {
            rect: ScreenRect {
                x: (self.rect.x as i64 + left) as i32,
                y: (self.rect.y as i64 + top) as i32,
                width: (right - left).max(1) as u32,
                height: (bottom - top).max(1) as u32,
            },
            width,
            height,
            rgba,
        })
    }
}

pub fn monitors() -> Result<Vec<ScreenRect>> {
    #[cfg(any(windows, target_os = "macos"))]
    {
        native::monitors()
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        bail!("当前平台暂不支持桌面截图")
    }
}

pub fn desktop_bounds() -> Result<ScreenRect> {
    let displays = monitors()?;
    ensure!(!displays.is_empty(), "未找到可截图的显示器");
    let x = displays.iter().map(|r| r.x).min().unwrap();
    let y = displays.iter().map(|r| r.y).min().unwrap();
    let right = displays
        .iter()
        .map(|r| r.x as i64 + r.width as i64)
        .max()
        .unwrap();
    let bottom = displays
        .iter()
        .map(|r| r.y as i64 + r.height as i64)
        .max()
        .unwrap();
    Ok(ScreenRect {
        x,
        y,
        width: (right - x as i64).try_into()?,
        height: (bottom - y as i64).try_into()?,
    })
}

pub fn capture_rect(rect: ScreenRect) -> Result<CaptureFrame> {
    capture_rect_with_cursor(rect, false)
}

pub fn capture_rect_with_cursor(rect: ScreenRect, include_cursor: bool) -> Result<CaptureFrame> {
    rect.validate()?;
    #[cfg(any(windows, target_os = "macos"))]
    {
        native::capture_rect(rect, include_cursor)
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        let _ = include_cursor;
        bail!("当前平台暂不支持桌面截图")
    }
}

pub fn png_bytes(frame: &CaptureFrame) -> Result<Vec<u8>> {
    frame.validate()?;
    let mut bytes = Vec::new();
    image::codecs::png::PngEncoder::new(&mut bytes).write_image(
        &frame.rgba,
        frame.width,
        frame.height,
        image::ExtendedColorType::Rgba8,
    )?;
    Ok(bytes)
}

pub fn save_png(frame: &CaptureFrame, path: &Path) -> Result<()> {
    frame.validate()?;
    let mut output = PendingFile::new(path)?;
    {
        let mut writer = BufWriter::new(&mut output.file);
        image::codecs::png::PngEncoder::new(&mut writer)
            .write_image(
                &frame.rgba,
                frame.width,
                frame.height,
                image::ExtendedColorType::Rgba8,
            )
            .context("无法写入 PNG 截图")?;
        writer.flush().context("无法完成 PNG 截图写入")?;
    }
    output.publish(path)
}

pub fn copy_image(frame: &CaptureFrame) -> Result<()> {
    frame.validate()?;
    #[cfg(any(windows, target_os = "macos"))]
    {
        native::copy_image(frame)
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        bail!("当前平台暂不支持复制截图")
    }
}

#[derive(Clone, Copy, Debug)]
pub struct RecordingOptions {
    pub fps: u16,
    pub max_seconds: u32,
    pub include_cursor: bool,
}

impl Default for RecordingOptions {
    fn default() -> Self {
        Self {
            fps: 10,
            max_seconds: 60,
            include_cursor: true,
        }
    }
}

#[derive(Debug)]
pub struct RecordingSummary {
    pub frames: u32,
    pub elapsed: Duration,
    pub path: PathBuf,
    pub width: u32,
    pub height: u32,
}

/// Capture and encode on a worker thread. Stop finalizes a valid GIF. Memory holds
/// at most two frames, rather than growing with the duration of a recording.
/// GIFs fit within 1920 px / 2 MP; PNG screenshots retain their original pixels.
pub fn record_gif(
    rect: ScreenRect,
    path: &Path,
    options: RecordingOptions,
    stop: Arc<AtomicBool>,
    elapsed_ms: Arc<AtomicU32>,
) -> Result<RecordingSummary> {
    rect.validate()?;
    ensure!((1..=30).contains(&options.fps), "GIF 帧率须为 1–30 FPS");
    ensure!(
        (1..=120).contains(&options.max_seconds),
        "GIF 录制时长须为 1–120 秒"
    );
    #[cfg(target_os = "macos")]
    {
        // Enumerate shareable content once per recording, with the controller
        // already visible, and retain the filter/configuration across frames.
        let mut session = native::CaptureSession::new(rect, options.include_cursor)?;
        session.require_own_windows_excluded()?;
        record_gif_with(path, options, stop, elapsed_ms, || session.capture())
    }
    #[cfg(not(target_os = "macos"))]
    {
        record_gif_with(path, options, stop, elapsed_ms, || {
            capture_rect_with_cursor(rect, options.include_cursor)
        })
    }
}

fn gif_dimensions(width: u32, height: u32) -> (u32, u32) {
    let scale = (MAX_GIF_EDGE as f64 / width.max(height) as f64)
        .min((MAX_GIF_PIXELS as f64 / (width as f64 * height as f64)).sqrt())
        .min(1.0);
    (
        ((width as f64 * scale).floor() as u32).max(1),
        ((height as f64 * scale).floor() as u32).max(1),
    )
}

fn gif_pixels(frame: CaptureFrame, width: u32, height: u32) -> Result<Vec<u8>> {
    frame.validate()?;
    if frame.width == width && frame.height == height {
        return Ok(frame.rgba);
    }
    let image =
        RgbaImage::from_raw(frame.width, frame.height, frame.rgba).context("无效的录制图像")?;
    Ok(
        image::imageops::resize(&image, width, height, image::imageops::FilterType::Triangle)
            .into_raw(),
    )
}

fn record_gif_with(
    path: &Path,
    options: RecordingOptions,
    stop: Arc<AtomicBool>,
    elapsed_ms: Arc<AtomicU32>,
    mut capture: impl FnMut() -> Result<CaptureFrame>,
) -> Result<RecordingSummary> {
    let mut output = PendingFile::new(path)?;
    let interval = Duration::from_secs_f64(1.0 / options.fps as f64);
    let duration = Duration::from_secs(options.max_seconds as u64);
    let started = Instant::now();
    let first = capture()?;
    first.validate()?;
    let (width, height) = gif_dimensions(first.width, first.height);
    let frames = {
        let writer = LimitedWriter {
            writer: BufWriter::new(&mut output.file),
            written: 0,
        };
        let mut encoder = gif::Encoder::new(writer, width as u16, height as u16, &[])?;
        encoder.set_repeat(gif::Repeat::Infinite)?;
        let mut pending = gif_pixels(first, width, height)?;
        let mut pending_at = Instant::now();
        let mut frames = 0;
        // Conservative bound for one indexed/LZW frame. Leave room to finish the
        // pending frame and trailer, rather than discarding a long recording at
        // the output limit. LimitedWriter remains a final hard limit on errors.
        let frame_reserve = width as u64 * height as u64 * 4 + 4096;
        loop {
            elapsed_ms.store(
                started.elapsed().as_millis().min(u32::MAX as u128) as u32,
                Ordering::Relaxed,
            );
            if stop.load(Ordering::Acquire)
                || started.elapsed() >= duration
                || encoder.get_ref().written + frame_reserve * 2 >= MAX_GIF_BYTES
            {
                break;
            }
            let due = pending_at + interval;
            while Instant::now() < due
                && !stop.load(Ordering::Acquire)
                && started.elapsed() < duration
            {
                std::thread::sleep(
                    due.saturating_duration_since(Instant::now())
                        .min(Duration::from_millis(20)),
                );
            }
            if stop.load(Ordering::Acquire) || started.elapsed() >= duration {
                break;
            }
            let captured_at = Instant::now();
            let next = gif_pixels(capture()?, width, height)?;
            write_gif_frame(
                &mut encoder,
                width,
                height,
                &mut pending,
                captured_at.duration_since(pending_at),
            )?;
            frames += 1;
            pending = next;
            pending_at = captured_at;
        }
        write_gif_frame(
            &mut encoder,
            width,
            height,
            &mut pending,
            pending_at.elapsed().max(interval),
        )?;
        frames += 1;
        // into_inner writes the trailer and propagates errors that Drop would discard.
        let writer = encoder.into_inner()?;
        writer
            .writer
            .into_inner()
            .map_err(|error| error.into_error())?
            .flush()?;
        frames
    };
    output.publish(path)?;
    let elapsed = started.elapsed();
    elapsed_ms.store(
        elapsed.as_millis().min(u32::MAX as u128) as u32,
        Ordering::Relaxed,
    );
    Ok(RecordingSummary {
        frames,
        elapsed,
        path: path.to_owned(),
        width,
        height,
    })
}

fn write_gif_frame<W: Write>(
    encoder: &mut gif::Encoder<W>,
    width: u32,
    height: u32,
    pixels: &mut [u8],
    duration: Duration,
) -> Result<()> {
    let mut frame = gif::Frame::from_rgba_speed(width as u16, height as u16, pixels, 20);
    frame.delay =
        ((duration.as_secs_f64() * 100.0).round() as u64).clamp(2, u16::MAX as u64) as u16;
    frame.dispose = gif::DisposalMethod::Keep;
    encoder.write_frame(&frame).context("GIF 编码失败")?;
    Ok(())
}

struct LimitedWriter<W> {
    writer: W,
    written: u64,
}
impl<W: Write> Write for LimitedWriter<W> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if self.written + bytes.len() as u64 > MAX_GIF_BYTES {
            return Err(io::Error::other(
                "GIF 已达到 256 MB 上限，请缩小区域或降低帧率",
            ));
        }
        let written = self.writer.write(bytes)?;
        self.written += written as u64;
        Ok(written)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.writer.flush()
    }
}

struct PendingFile {
    path: PathBuf,
    file: File,
}
impl PendingFile {
    fn new(destination: &Path) -> Result<Self> {
        ensure!(
            !destination.exists(),
            "目标文件已存在：{}",
            destination.display()
        );
        let parent = destination
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .context("请选择有效的输出目录")?;
        fs::create_dir_all(parent)
            .with_context(|| format!("无法创建截图目录：{}", parent.display()))?;
        for _ in 0..16 {
            let path = parent.join(format!(
                ".moin-capture-{}-{}.part",
                std::process::id(),
                NEXT_FILE.fetch_add(1, Ordering::Relaxed)
            ));
            match OpenOptions::new().write(true).create_new(true).open(&path) {
                Ok(file) => return Ok(Self { path, file }),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error).context("无法创建截图输出文件"),
            }
        }
        bail!("无法创建截图临时文件")
    }

    fn publish(&mut self, destination: &Path) -> Result<()> {
        self.file.flush()?;
        self.file.sync_all()?;
        // A hard link publishes atomically and never replaces an existing file.
        // FAT/network volumes may not support links; create_new still preserves data.
        if fs::hard_link(&self.path, destination).is_err() {
            let mut final_file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(destination)
                .context("无法保存截图；文件已存在或目录不可写")?;
            let result = (|| -> io::Result<()> {
                io::copy(&mut File::open(&self.path)?, &mut final_file)?;
                final_file.sync_all()
            })();
            if let Err(error) = result {
                drop(final_file);
                let _ = fs::remove_file(destination);
                return Err(error).context("保存截图失败");
            }
        }
        Ok(())
    }
}
impl Drop for PendingFile {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn sample() -> CaptureFrame {
        CaptureFrame {
            rect: ScreenRect {
                x: -100,
                y: 20,
                width: 2,
                height: 2,
            },
            width: 4,
            height: 4,
            rgba: (0..16).flat_map(|n| [n, 255 - n, n * 2, 255]).collect(),
        }
    }
    #[test]
    fn crop_retains_retina_and_negative_origin_mapping() {
        let image = sample();
        let cropped = image.crop(2, 0, 2, 4).unwrap();
        assert_eq!(
            cropped.rect,
            ScreenRect {
                x: -99,
                y: 20,
                width: 1,
                height: 2
            }
        );
        assert_eq!(&cropped.rgba[..8], &image.rgba[8..16]);
        assert!(image.crop(3, 0, 2, 4).is_err());
        assert!(image.crop(0, 0, 0, 4).is_err());
    }
    #[test]
    fn png_roundtrips_and_never_overwrites_existing_files() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("shot.png");
        let image = sample();
        save_png(&image, &path).unwrap();
        let decoded = image::open(&path).unwrap().into_rgba8();
        assert_eq!(decoded.dimensions(), (4, 4));
        assert_eq!(decoded.into_raw(), image.rgba);
        assert!(save_png(&image, &path).is_err());
        assert_eq!(fs::read_dir(directory.path()).unwrap().count(), 1);
    }
    #[test]
    fn stopped_recording_is_valid_gif_and_failure_removes_partial_output() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("clip.gif");
        let stopped = Arc::new(AtomicBool::new(true));
        let summary = record_gif_with(
            &path,
            RecordingOptions::default(),
            stopped.clone(),
            Arc::new(AtomicU32::new(0)),
            || Ok(sample()),
        )
        .unwrap();
        assert_eq!(summary.frames, 1);
        let mut decoder = gif::DecodeOptions::new();
        decoder.set_color_output(gif::ColorOutput::RGBA);
        let mut decoded = decoder.read_info(File::open(&path).unwrap()).unwrap();
        assert_eq!((decoded.width(), decoded.height()), (4, 4));
        assert!(decoded.read_next_frame().unwrap().is_some());
        assert!(decoded.read_next_frame().unwrap().is_none());
        let failed = directory.path().join("failure.gif");
        assert!(
            record_gif_with(
                &failed,
                RecordingOptions::default(),
                stopped,
                Arc::new(AtomicU32::new(0)),
                || bail!("synthetic failure")
            )
            .is_err()
        );
        assert!(!failed.exists());
        assert_eq!(fs::read_dir(directory.path()).unwrap().count(), 1);
    }
    #[test]
    fn gif_resolution_is_bounded_and_preserves_aspect() {
        assert_eq!(gif_dimensions(3840, 2160), (1920, 1080));
        assert_eq!(gif_dimensions(800, 600), (800, 600));
        let (width, height) = gif_dimensions(8000, 8000);
        assert!(width as u64 * height as u64 <= MAX_GIF_PIXELS);
    }

    #[test]
    fn gif_streams_multiple_frames_and_discards_failures_after_first_frame() {
        let directory = tempfile::tempdir().unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let mut calls = 0;
        let path = directory.path().join("animated.gif");
        let summary = record_gif_with(
            &path,
            RecordingOptions {
                fps: 30,
                ..Default::default()
            },
            stop.clone(),
            Arc::new(AtomicU32::new(0)),
            || {
                calls += 1;
                let mut frame = sample();
                for pixel in frame.rgba.chunks_exact_mut(4) {
                    pixel.copy_from_slice(&[calls * 50, 0, 0, 255]);
                }
                if calls == 3 {
                    stop.store(true, Ordering::Release);
                }
                Ok(frame)
            },
        )
        .unwrap();
        assert_eq!(summary.frames, 3);
        let mut options = gif::DecodeOptions::new();
        options.set_color_output(gif::ColorOutput::RGBA);
        let mut reader = options.read_info(File::open(&path).unwrap()).unwrap();
        for red in [50, 100, 150] {
            let frame = reader.read_next_frame().unwrap().unwrap();
            assert_eq!(&frame.buffer[..4], &[red, 0, 0, 255]);
            assert!(frame.delay >= 2);
        }
        assert!(reader.read_next_frame().unwrap().is_none());
        let failed = directory.path().join("failed.gif");
        let mut calls = 0;
        assert!(
            record_gif_with(
                &failed,
                RecordingOptions {
                    fps: 30,
                    ..Default::default()
                },
                Arc::new(AtomicBool::new(false)),
                Arc::new(AtomicU32::new(0)),
                || {
                    calls += 1;
                    if calls == 2 {
                        bail!("synthetic late capture failure");
                    }
                    Ok(sample())
                }
            )
            .is_err()
        );
        assert!(!failed.exists());
        assert_eq!(fs::read_dir(directory.path()).unwrap().count(), 1);
    }
}
