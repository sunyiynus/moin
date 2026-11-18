//! AppKit menu bar host. Native objects stay on GPUI's main thread; callbacks
//! only send events, so neither Carbon nor AppKit callbacks re-enter GPUI.

use anyhow::{Context, Result, bail};
use async_channel::{Receiver, Sender};
use cocoa::{
    appkit::{NSApp, NSApplication, NSApplicationActivationPolicy},
    base::YES,
};
use fs2::FileExt;
use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState, hotkey::HotKey};
use moin::hotkey::Shortcut;
use std::{
    cell::RefCell,
    collections::hash_map::DefaultHasher,
    fs::{self, File, OpenOptions},
    hash::{Hash, Hasher},
    io,
    os::unix::{
        fs::{FileTypeExt, MetadataExt, PermissionsExt},
        net::UnixDatagram,
    },
    path::{Path, PathBuf},
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU32, Ordering},
    },
    thread::{self, JoinHandle},
    time::Duration,
};
use tray_icon::{
    Icon, MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent,
    menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem},
};

pub enum TrayEvent {
    Toggle,
    Show,
    Quit,
    HotkeyChanged(Option<String>),
}

pub enum TrayStart {
    Ready(Tray, Receiver<TrayEvent>),
    AlreadyRunning,
}

pub struct Tray {
    host: Rc<RefCell<Host>>,
}

#[derive(Clone)]
pub struct TrayController {
    host: Rc<RefCell<Host>>,
}

impl Tray {
    pub fn controller(&self) -> TrayController {
        TrayController {
            host: self.host.clone(),
        }
    }
}

impl TrayController {
    pub fn change_hotkey(&self, text: &str) -> Result<Receiver<Result<String>>> {
        self.change(Some(Shortcut::parse(text)?))
    }

    pub fn disable_hotkey(&self) -> Result<Receiver<Result<String>>> {
        self.change(None)
    }

    fn change(&self, shortcut: Option<Shortcut>) -> Result<Receiver<Result<String>>> {
        // Controllers live in UI entities and cannot leave the AppKit thread.
        // Keep the Windows asynchronous response contract for settings rollback.
        let (response, receiver) = async_channel::bounded(1);
        let result = self.host.borrow_mut().replace_shortcut(shortcut);
        let _ = response.try_send(result);
        Ok(receiver)
    }
}

struct Host {
    manager: GlobalHotKeyManager,
    registered: Option<(Shortcut, HotKey)>,
    registered_id: Arc<AtomicU32>,
    pressed_id: Arc<AtomicU32>,
    alive: Arc<AtomicBool>,
    events: Sender<TrayEvent>,
    icon: TrayIcon,
    _instance: SingleInstance,
}

impl Host {
    fn replace_shortcut(&mut self, shortcut: Option<Shortcut>) -> Result<String> {
        if self.registered.map(|(key, _)| key) == shortcut {
            return Ok(shortcut.map(|key| key.label()).unwrap_or_default());
        }
        let next = shortcut
            .map(|shortcut| {
                let hotkey = shortcut
                    .label()
                    .parse::<HotKey>()
                    .context("macOS 无法识别这个快捷键")?;
                self.manager
                    .register(hotkey)
                    .context("快捷键被系统或其他应用占用，请选择其他组合")?;
                Ok::<_, anyhow::Error>((shortcut, hotkey))
            })
            .transpose()?;
        // Register first: conflicts keep the old shortcut working.
        if let Some((_, old)) = self.registered
            && let Err(error) = self.manager.unregister(old)
        {
            if let Some((_, new)) = next {
                let _ = self.manager.unregister(new);
            }
            return Err(error).context("无法释放旧快捷键，已保留原设置");
        }
        self.registered = next;
        self.registered_id
            .store(next.map_or(0, |(_, key)| key.id()), Ordering::Release);
        self.pressed_id.store(0, Ordering::Release);
        let label = next.map(|(shortcut, _)| shortcut.label());
        let tooltip = label
            .as_ref()
            .map_or_else(|| "Moin · 点击打开".into(), |key| format!("Moin · {key}"));
        let _ = self.icon.set_tooltip(Some(tooltip));
        let _ = self
            .events
            .try_send(TrayEvent::HotkeyChanged(label.clone()));
        Ok(label.unwrap_or_default())
    }
}

impl Drop for Host {
    fn drop(&mut self) {
        self.alive.store(false, Ordering::Release);
        self.registered_id.store(0, Ordering::Release);
        if let Some((_, hotkey)) = self.registered {
            let _ = self.manager.unregister(hotkey);
        }
        // Upstream event handlers are process-global OnceCells: setting None
        // cannot replace them. The alive flag makes retained callbacks inert;
        // they capture channels/atomics only, never native objects or the host.
    }
}

/// Called from GPUI's launch callback, after AppKit starts its main run loop.
pub fn start(shortcut: &str, home: &Path) -> Result<TrayStart> {
    let (events, receiver) = async_channel::unbounded();
    let Some(instance) = SingleInstance::acquire(home, events.clone())? else {
        return Ok(TrayStart::AlreadyRunning);
    };
    let shortcut = Shortcut::parse(shortcut).context("激活快捷键配置无效")?;
    unsafe {
        // Accessory applications have menu bar windows and no Dock icon.
        if NSApp().setActivationPolicy_(
            NSApplicationActivationPolicy::NSApplicationActivationPolicyAccessory,
        ) != YES
        {
            bail!("无法将 Moin 设为 macOS 菜单栏应用");
        }
    }
    let manager = GlobalHotKeyManager::new().context("无法创建 macOS 快捷键服务")?;
    let registered_id = Arc::new(AtomicU32::new(0));
    let pressed_id = Arc::new(AtomicU32::new(0));
    let alive = Arc::new(AtomicBool::new(true));
    let active = registered_id.clone();
    let pressed = pressed_id.clone();
    let hotkey_alive = alive.clone();
    let hotkey_events = events.clone();
    GlobalHotKeyEvent::set_event_handler(Some(move |event: GlobalHotKeyEvent| {
        if !hotkey_alive.load(Ordering::Acquire) || event.id != active.load(Ordering::Acquire) {
            return;
        }
        match event.state {
            HotKeyState::Pressed => {
                if pressed
                    .compare_exchange(0, event.id, Ordering::AcqRel, Ordering::Acquire)
                    .is_ok()
                {
                    let _ = hotkey_events.try_send(TrayEvent::Toggle);
                }
            }
            HotKeyState::Released => {
                pressed.store(0, Ordering::Release);
            }
        }
    }));

    let open = MenuItem::new("打开 Moin", true, None);
    let quit = MenuItem::new("退出 Moin", true, None);
    let menu = Menu::new();
    menu.append_items(&[&open, &PredefinedMenuItem::separator(), &quit])
        .context("无法创建 macOS 菜单栏菜单")?;
    let open_id = open.id().clone();
    let quit_id = quit.id().clone();
    let menu_events = events.clone();
    let menu_alive = alive.clone();
    MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
        if !menu_alive.load(Ordering::Acquire) {
            return;
        }
        let event = if event.id == open_id {
            TrayEvent::Show
        } else if event.id == quit_id {
            TrayEvent::Quit
        } else {
            return;
        };
        let _ = menu_events.try_send(event);
    }));

    // Template icons automatically follow the menu bar's light/dark appearance.
    let icon = TrayIconBuilder::new()
        .with_id("moin-menu-bar")
        .with_icon_templated(mark()?)
        .with_tooltip("Moin")
        .with_autosave_name("Moin.MenuBar")
        .with_menu(Box::new(menu))
        .with_menu_on_left_click(false)
        .build()
        .context("无法添加 macOS 菜单栏图标")?;
    let tray_id = icon.id().clone();
    let click_events = events.clone();
    let click_alive = alive.clone();
    TrayIconEvent::set_event_handler(Some(move |event: TrayIconEvent| {
        if !click_alive.load(Ordering::Acquire) {
            return;
        }
        if let TrayIconEvent::Click {
            id,
            button: MouseButton::Left,
            button_state: MouseButtonState::Up,
            ..
        } = event
            && id == tray_id
        {
            let _ = click_events.try_send(TrayEvent::Show);
        }
    }));
    let mut host = Host {
        manager,
        registered: None,
        registered_id,
        pressed_id,
        alive,
        events,
        icon,
        _instance: instance,
    };
    if let Err(error) = host.replace_shortcut(Some(shortcut)) {
        // A busy system shortcut must not make the launcher inaccessible.
        eprintln!("无法注册激活快捷键 · {error:#}");
        let _ = host.events.try_send(TrayEvent::HotkeyChanged(None));
    }
    Ok(TrayStart::Ready(
        Tray {
            // The UI's controller retains the host after the launch callback.
            host: Rc::new(RefCell::new(host)),
        },
        receiver,
    ))
}

fn mark() -> Result<Icon> {
    const SIZE: usize = 24;
    let mut pixels = vec![0u8; SIZE * SIZE * 4];
    for y in 0..SIZE {
        for x in 0..SIZE {
            if ((4..8).contains(&x) && (8..17).contains(&y))
                || ((10..14).contains(&x) && (4..21).contains(&y))
                || ((16..20).contains(&x) && (7..18).contains(&y))
            {
                pixels[(y * SIZE + x) * 4 + 3] = 255;
            }
        }
    }
    Icon::from_rgba(pixels, SIZE as u32, SIZE as u32).context("无法绘制 Moin 菜单栏图标")
}

/// OS-owned file locks survive crashes correctly; the datagram socket only
/// transports a show request from a second launch to the resident process.
struct SingleInstance {
    _lock: File,
    socket_path: PathBuf,
    stopped: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

impl SingleInstance {
    fn acquire(home: &Path, events: Sender<TrayEvent>) -> Result<Option<Self>> {
        fs::create_dir_all(home)?;
        let home = home.canonicalize()?;
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(home.join("moin.lock"))
            .context("无法创建 Moin 实例锁")?;
        let mut hash = DefaultHasher::new();
        home.hash(&mut hash);
        let uid = home.metadata()?.uid();
        // Darwin's sockaddr_un has a short path limit. /tmp keeps the wake
        // socket short even with a long application-data directory or username.
        let socket_path = PathBuf::from(format!("/tmp/moin-{uid}-{:016x}.sock", hash.finish()));
        match FileExt::try_lock_exclusive(&lock) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                let socket = UnixDatagram::unbound()?;
                for attempt in 0..20 {
                    match socket.send_to(b"show", &socket_path) {
                        Ok(_) => return Ok(None),
                        Err(_) if attempt < 19 => thread::sleep(Duration::from_millis(25)),
                        Err(error) => return Err(error).context("无法唤起已运行的 Moin"),
                    }
                }
                unreachable!();
            }
            Err(error) => return Err(error).context("无法检查 Moin 实例锁"),
        }
        match fs::symlink_metadata(&socket_path) {
            Ok(metadata) if metadata.file_type().is_socket() => fs::remove_file(&socket_path)?,
            Ok(_) => bail!("Moin 唤起套接字路径已被其他文件占用"),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        let socket = UnixDatagram::bind(&socket_path).context("无法创建 Moin 唤起套接字")?;
        fs::set_permissions(&socket_path, fs::Permissions::from_mode(0o600))?;
        socket.set_read_timeout(Some(Duration::from_millis(250)))?;
        let stopped = Arc::new(AtomicBool::new(false));
        let stop = stopped.clone();
        let worker = thread::Builder::new()
            .name("moin-wake".into())
            .spawn(move || {
                let mut buffer = [0; 16];
                while !stop.load(Ordering::Acquire) {
                    match socket.recv(&mut buffer) {
                        Ok(size) if &buffer[..size] == b"show" => {
                            let _ = events.try_send(TrayEvent::Show);
                        }
                        Ok(_) => {}
                        Err(error)
                            if matches!(
                                error.kind(),
                                io::ErrorKind::WouldBlock
                                    | io::ErrorKind::TimedOut
                                    | io::ErrorKind::Interrupted
                            ) => {}
                        Err(_) => break,
                    }
                }
            })
            .context("无法创建 Moin 唤起线程")?;
        Ok(Some(Self {
            _lock: lock,
            socket_path,
            stopped,
            worker: Some(worker),
        }))
    }
}

impl Drop for SingleInstance {
    fn drop(&mut self) {
        self.stopped.store(true, Ordering::Release);
        if let Ok(socket) = UnixDatagram::unbound() {
            let _ = socket.send_to(b"stop", &self.socket_path);
        }
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
        let _ = fs::remove_file(&self.socket_path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn second_instance_wakes_resident_and_lock_releases_on_drop() {
        let home = tempfile::tempdir().unwrap();
        let (events, receiver) = async_channel::unbounded();
        let first = SingleInstance::acquire(home.path(), events.clone())
            .unwrap()
            .unwrap();
        assert!(
            SingleInstance::acquire(home.path(), events.clone())
                .unwrap()
                .is_none()
        );
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        loop {
            match receiver.try_recv() {
                Ok(TrayEvent::Show) => break,
                Err(async_channel::TryRecvError::Empty) if std::time::Instant::now() < deadline => {
                    thread::sleep(Duration::from_millis(10));
                }
                _ => panic!("second launch did not wake the resident process"),
            }
        }
        let socket = first.socket_path.clone();
        drop(first);
        assert!(!socket.exists());
        assert!(
            SingleInstance::acquire(home.path(), events)
                .unwrap()
                .is_some()
        );
    }

    #[test]
    fn mac_shortcut_labels_match_native_hotkey_parser() {
        for shortcut in [
            "Alt+Space",
            "Cmd+K",
            "Ctrl+Alt+F12",
            "Cmd+Shift+;",
            "Option+Down",
        ] {
            let shortcut = Shortcut::parse(shortcut).unwrap();
            assert!(shortcut.label().parse::<HotKey>().is_ok());
        }
    }
}
