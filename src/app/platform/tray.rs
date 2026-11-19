use anyhow::{Context, Result, bail};
use async_channel::{Receiver, Sender};
use moin::hotkey::{FeatureAction, FeatureShortcuts, Shortcut};
use std::{
    cell::{Cell, RefCell},
    collections::{HashSet, VecDeque},
    ffi::c_void,
    mem::size_of,
    os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle},
    ptr::{self, null_mut},
    sync::{
        Arc, Mutex,
        mpsc::{SyncSender, sync_channel},
    },
    thread::{self, JoinHandle},
};
use windows::{
    Win32::{
        Foundation::{
            ERROR_ALREADY_EXISTS, GetLastError, HANDLE, HINSTANCE, HWND, LPARAM, LRESULT, POINT,
            WAIT_OBJECT_0, WPARAM,
        },
        Graphics::Gdi::{
            BI_RGB, BITMAPINFO, BITMAPINFOHEADER, CreateBitmap, CreateDIBSection, DIB_RGB_COLORS,
            DeleteObject,
        },
        System::{
            LibraryLoader::GetModuleHandleW,
            Threading::{CreateEventW, INFINITE, SetEvent},
        },
        UI::{
            Input::KeyboardAndMouse::{
                HOT_KEY_MODIFIERS, MOD_NOREPEAT, RegisterHotKey, UnregisterHotKey,
            },
            Shell::{
                NIF_ICON, NIF_MESSAGE, NIF_SHOWTIP, NIF_TIP, NIM_ADD, NIM_DELETE, NIM_MODIFY,
                NIM_SETFOCUS, NIM_SETVERSION, NIN_SELECT, NINF_KEY, NOTIFYICON_VERSION_4,
                NOTIFYICONDATAW, NOTIFYICONDATAW_0, Shell_NotifyIconW,
            },
            WindowsAndMessaging::*,
        },
    },
    core::{PCWSTR, w},
};

const WINDOW_CLASS: PCWSTR = w!("Moin.Tray.06d98f42");
const INSTANCE_EVENT: PCWSTR = w!("Local\\Moin.Tray.Show.06d98f42");
const TRAY_CALLBACK: u32 = WM_APP + 1;
const CHANGE_HOTKEY: u32 = WM_APP + 2;
const ICON_ID: u32 = 1;
const HOTKEY_ID: i32 = 1;
const MENU_OPEN: usize = 1;
const MENU_QUIT: usize = 2;
const NIN_KEYSELECT: u32 = NIN_SELECT | NINF_KEY;

pub enum TrayEvent {
    Toggle,
    Show,
    Quit,
    HotkeyChanged(Option<String>),
    ActivateFeature(FeatureAction),
}

pub enum TrayStart {
    Ready(Tray, Receiver<TrayEvent>),
    AlreadyRunning,
}

pub struct Tray {
    hwnd: HWND,
    controller: TrayController,
    worker: Option<JoinHandle<()>>,
}

enum HotkeyRequest {
    Activation {
        shortcut: Option<Shortcut>,
        response: Sender<Result<String>>,
    },
    Features {
        shortcuts: Vec<(FeatureAction, Shortcut)>,
        response: Sender<Result<()>>,
    },
}

#[derive(Clone)]
pub struct TrayController {
    hwnd: isize,
    requests: Arc<Mutex<VecDeque<HotkeyRequest>>>,
}

impl TrayController {
    pub fn change_hotkey(&self, shortcut: &str) -> Result<Receiver<Result<String>>> {
        self.queue_hotkey(Some(Shortcut::parse(shortcut)?))
    }

    /// Restore a previous state with no registered shortcut after a failed settings save.
    pub fn disable_hotkey(&self) -> Result<Receiver<Result<String>>> {
        self.queue_hotkey(None)
    }

    pub fn change_feature_hotkeys(
        &self,
        shortcuts: &FeatureShortcuts,
    ) -> Result<Receiver<Result<()>>> {
        let shortcuts = shortcuts.parsed()?;
        let (response, receiver) = async_channel::bounded(1);
        self.queue_request(HotkeyRequest::Features {
            shortcuts,
            response,
        })?;
        Ok(receiver)
    }

    fn queue_hotkey(&self, shortcut: Option<Shortcut>) -> Result<Receiver<Result<String>>> {
        let (response, receiver) = async_channel::bounded(1);
        self.queue_request(HotkeyRequest::Activation { shortcut, response })?;
        Ok(receiver)
    }

    fn queue_request(&self, request: HotkeyRequest) -> Result<()> {
        let mut requests = self
            .requests
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        requests.push_back(request);
        let posted = unsafe {
            PostMessageW(
                Some(HWND(self.hwnd as *mut c_void)),
                CHANGE_HOTKEY,
                WPARAM(0),
                LPARAM(0),
            )
        };
        if let Err(error) = posted {
            requests.pop_back();
            return Err(error).context("系统托盘已关闭，无法修改快捷键");
        }
        Ok(())
    }
}

impl Tray {
    pub fn controller(&self) -> TrayController {
        self.controller.clone()
    }
}

impl Drop for Tray {
    fn drop(&mut self) {
        unsafe {
            // A synchronous close also reaches the tray host while it is tracking its menu.
            let _ = SendMessageTimeoutW(
                self.hwnd,
                WM_CLOSE,
                WPARAM(0),
                LPARAM(0),
                SMTO_ABORTIFHUNG,
                1500,
                None,
            );
        }
        if let Some(worker) = self.worker.take()
            && worker.is_finished()
        {
            let _ = worker.join();
        }
    }
}

pub fn start(shortcut: &str) -> Result<TrayStart> {
    let (show, existing) = unsafe {
        let handle =
            CreateEventW(None, false, false, INSTANCE_EVENT).context("无法创建 Moin 唤起事件")?;
        let existing = GetLastError() == ERROR_ALREADY_EXISTS;
        (OwnedHandle::from_raw_handle(handle.0), existing)
    };
    if existing {
        grant_foreground();
        unsafe { SetEvent(HANDLE(show.as_raw_handle())) }.context("无法唤起 Moin 搜索窗口")?;
        return Ok(TrayStart::AlreadyRunning);
    }

    let shortcut = Shortcut::parse(shortcut).context("激活快捷键配置无效")?;

    let (events, receiver) = async_channel::unbounded();
    let requests = Arc::new(Mutex::new(VecDeque::new()));
    let host_requests = requests.clone();
    let (ready, startup) = sync_channel(1);
    let worker = thread::Builder::new()
        .name("moin-tray".into())
        .spawn(move || {
            if let Err(error) = run_host(&ready, events, show, shortcut, host_requests) {
                let _ = ready.send(Err(error));
            }
        })
        .context("无法创建系统托盘线程")?;
    let hwnd = HWND(startup.recv().context("系统托盘启动失败")?? as *mut c_void);
    Ok(TrayStart::Ready(
        Tray {
            hwnd,
            controller: TrayController {
                hwnd: hwnd.0 as isize,
                requests,
            },
            worker: Some(worker),
        },
        receiver,
    ))
}

fn grant_foreground() {
    unsafe {
        if let Ok(hwnd) = FindWindowW(WINDOW_CLASS, PCWSTR::null()) {
            let mut process = 0;
            GetWindowThreadProcessId(hwnd, Some(&mut process));
            if process != 0 {
                let _ = AllowSetForegroundWindow(process);
            }
        }
    }
}

struct WindowClass(HINSTANCE);

impl Drop for WindowClass {
    fn drop(&mut self) {
        unsafe {
            let _ = UnregisterClassW(WINDOW_CLASS, Some(self.0));
        }
    }
}

struct Host {
    events: Sender<TrayEvent>,
    icon: TrayIcon,
    taskbar_created: u32,
    hotkey: Cell<Option<RegisteredHotkey>>,
    features: RefCell<Vec<RegisteredFeature>>,
    requests: Arc<Mutex<VecDeque<HotkeyRequest>>>,
    version_four: Cell<bool>,
}

impl Host {
    fn icon_data(&self, hwnd: HWND) -> NOTIFYICONDATAW {
        let mut tip = [0; 128];
        let text = self.hotkey.get().map_or_else(
            || "Moin · 点击打开".into(),
            |registered| format!("Moin · {}", registered.shortcut.label()),
        );
        for (dest, src) in tip.iter_mut().zip(text.encode_utf16()) {
            *dest = src;
        }
        NOTIFYICONDATAW {
            cbSize: size_of::<NOTIFYICONDATAW>() as u32,
            hWnd: hwnd,
            uID: ICON_ID,
            uFlags: NIF_ICON | NIF_MESSAGE | NIF_TIP | NIF_SHOWTIP,
            uCallbackMessage: TRAY_CALLBACK,
            hIcon: self.icon.handle,
            szTip: tip,
            Anonymous: NOTIFYICONDATAW_0 {
                uVersion: NOTIFYICON_VERSION_4,
            },
            ..Default::default()
        }
    }

    fn show_icon(&self, hwnd: HWND) -> bool {
        let data = self.icon_data(hwnd);
        unsafe {
            if Shell_NotifyIconW(NIM_ADD, &data).as_bool() {
                self.version_four
                    .set(Shell_NotifyIconW(NIM_SETVERSION, &data).as_bool());
                true
            } else {
                Shell_NotifyIconW(NIM_MODIFY, &data).as_bool()
            }
        }
    }

    fn emit(&self, event: TrayEvent) {
        let _ = self.events.try_send(event);
    }

    fn change_hotkeys(&self, hwnd: HWND) {
        loop {
            let request = self
                .requests
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .pop_front();
            let Some(request) = request else {
                return;
            };
            match request {
                HotkeyRequest::Features {
                    shortcuts,
                    response,
                } => {
                    let result = replace_feature_shortcuts(
                        hwnd,
                        &mut self.features.borrow_mut(),
                        &shortcuts,
                        self.hotkey.get().map(|key| key.shortcut),
                    );
                    let _ = response.try_send(result);
                }
                HotkeyRequest::Activation { shortcut, response } => {
                    let result = if let Some(shortcut) = shortcut {
                        if self
                            .features
                            .borrow()
                            .iter()
                            .any(|feature| feature.registered.shortcut == shortcut)
                        {
                            Err(anyhow::anyhow!(
                                "{} 已分配给功能快捷键，请先修改该功能",
                                shortcut.label()
                            ))
                        } else {
                            replace_shortcut(hwnd, self.hotkey.get(), shortcut).map(Some)
                        }
                    } else {
                        match self.hotkey.get() {
                            Some(registered) => {
                                unsafe { UnregisterHotKey(Some(hwnd), registered.id) }
                                    .context("无法恢复快捷键状态")
                                    .map(|()| None)
                            }
                            None => Ok(None),
                        }
                    };
                    let result = match result {
                        Ok(registered) => {
                            self.hotkey.set(registered);
                            let label = registered.map(|registered| registered.shortcut.label());
                            unsafe {
                                let _ = Shell_NotifyIconW(NIM_MODIFY, &self.icon_data(hwnd));
                            }
                            self.emit(TrayEvent::HotkeyChanged(label.clone()));
                            Ok(label.unwrap_or_default())
                        }
                        Err(error) => Err(error),
                    };
                    let _ = response.try_send(result);
                }
            }
        }
    }

    fn menu(&self, hwnd: HWND, coords: WPARAM) {
        let mut point = POINT::default();
        unsafe {
            if self.version_four.get() {
                point.x = (coords.0 as u16 as i16).into();
                point.y = ((coords.0 >> 16) as u16 as i16).into();
            } else if GetCursorPos(&mut point).is_err() {
                return;
            }

            let Ok(menu) = CreatePopupMenu() else {
                return;
            };
            let items = AppendMenuW(menu, MF_STRING, MENU_OPEN, w!("打开 Moin"))
                .and_then(|_| AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null()))
                .and_then(|_| AppendMenuW(menu, MF_STRING, MENU_QUIT, w!("退出 Moin")));
            if items.is_ok() {
                let _ = SetForegroundWindow(hwnd);
                let command = TrackPopupMenu(
                    menu,
                    TPM_RETURNCMD | TPM_NONOTIFY | TPM_RIGHTBUTTON | TPM_BOTTOMALIGN,
                    point.x,
                    point.y,
                    None,
                    hwnd,
                    None,
                );
                let _ = PostMessageW(Some(hwnd), WM_NULL, WPARAM(0), LPARAM(0));
                match command.0 as usize {
                    MENU_OPEN => self.emit(TrayEvent::Show),
                    MENU_QUIT => self.emit(TrayEvent::Quit),
                    _ => {
                        let _ = Shell_NotifyIconW(NIM_SETFOCUS, &self.icon_data(hwnd));
                    }
                }
            }
            let _ = DestroyMenu(menu);
        }
    }
}

fn run_host(
    ready: &SyncSender<Result<isize>>,
    events: Sender<TrayEvent>,
    show: OwnedHandle,
    shortcut: Shortcut,
    requests: Arc<Mutex<VecDeque<HotkeyRequest>>>,
) -> Result<()> {
    unsafe {
        let instance = HINSTANCE(GetModuleHandleW(PCWSTR::null())?.0);
        let class = WNDCLASSW {
            hInstance: instance,
            lpszClassName: WINDOW_CLASS,
            lpfnWndProc: Some(window_proc),
            ..Default::default()
        };
        if RegisterClassW(&class) == 0 {
            return Err(windows::core::Error::from_win32()).context("无法注册托盘消息窗口");
        }
        let _class = WindowClass(instance);
        // The hidden top-level window receives Explorer restart broadcasts.
        let host = Box::new(Host {
            events,
            icon: TrayIcon::new()?,
            taskbar_created: RegisterWindowMessageW(w!("TaskbarCreated")),
            hotkey: Cell::new(None),
            features: RefCell::new(Vec::new()),
            requests,
            version_four: Cell::new(false),
        });
        let hwnd = CreateWindowExW(
            WS_EX_TOOLWINDOW,
            WINDOW_CLASS,
            WINDOW_CLASS,
            WS_POPUP,
            0,
            0,
            0,
            0,
            None,
            None,
            Some(instance),
            Some(ptr::from_ref(host.as_ref()).cast()),
        )
        .context("无法创建托盘消息窗口")?;

        let hotkey = register_shortcut(hwnd, shortcut);
        host.hotkey.set(hotkey);
        if !host.show_icon(hwnd) {
            let _ = DestroyWindow(hwnd);
            bail!("无法在系统通知区域添加 Moin 图标");
        }
        host.emit(TrayEvent::HotkeyChanged(
            hotkey.map(|registered| registered.shortcut.label()),
        ));
        if ready.send(Ok(hwnd.0 as isize)).is_err() {
            let _ = DestroyWindow(hwnd);
            return Ok(());
        }
        let event = [HANDLE(show.as_raw_handle())];
        'messages: loop {
            let reason = MsgWaitForMultipleObjectsEx(
                Some(&event),
                INFINITE,
                QS_ALLINPUT,
                MWMO_INPUTAVAILABLE,
            );
            if reason == WAIT_OBJECT_0 {
                host.emit(TrayEvent::Show);
            } else if reason.0 != WAIT_OBJECT_0.0 + 1 {
                break;
            }

            let mut message = MSG::default();
            while PeekMessageW(&mut message, None, 0, 0, PM_REMOVE).as_bool() {
                if message.message == WM_QUIT {
                    break 'messages;
                }
                let _ = TranslateMessage(&message);
                DispatchMessageW(&message);
            }
        }
        if IsWindow(Some(hwnd)).as_bool() {
            let _ = DestroyWindow(hwnd);
        }
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct RegisteredHotkey {
    id: i32,
    shortcut: Shortcut,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct RegisteredFeature {
    action: FeatureAction,
    registered: RegisteredHotkey,
}

/// Reuse native registrations by key combination, not action. This allows
/// exchanging two feature keys without unregistering either key first.
fn replace_feature_shortcuts(
    hwnd: HWND,
    current: &mut Vec<RegisteredFeature>,
    desired: &[(FeatureAction, Shortcut)],
    activation: Option<Shortcut>,
) -> Result<()> {
    let mut keys = HashSet::new();
    let mut actions = HashSet::new();
    for &(action, shortcut) in desired {
        anyhow::ensure!(actions.insert(action), "功能快捷键重复：{}", action.label());
        anyhow::ensure!(
            keys.insert(shortcut),
            "快捷键 {} 已分配给其他功能",
            shortcut.label()
        );
        anyhow::ensure!(
            Some(shortcut) != activation,
            "{} 与主激活快捷键冲突",
            shortcut.label()
        );
    }
    let mut next = Vec::with_capacity(desired.len());
    let mut staged = Vec::<RegisteredHotkey>::new();
    for &(action, shortcut) in desired {
        let registered = if let Some(existing) = current
            .iter()
            .find(|feature| feature.registered.shortcut == shortcut)
        {
            existing.registered
        } else {
            // Main activation uses IDs 1/2. These IDs belong exclusively to the
            // feature registry and are released before the next transaction.
            let id = (100..0xbfff)
                .find(|id| {
                    !current.iter().any(|feature| feature.registered.id == *id)
                        && !staged.iter().any(|key| key.id == *id)
                })
                .context("功能快捷键数量过多")?;
            if let Err(error) = register(hwnd, id, shortcut) {
                for key in &staged {
                    unsafe {
                        let _ = UnregisterHotKey(Some(hwnd), key.id);
                    }
                }
                return Err(error).with_context(|| {
                    format!(
                        "{}：{} 已被占用或无法注册，原快捷键保持生效",
                        action.label(),
                        shortcut.label()
                    )
                });
            }
            let key = RegisteredHotkey { id, shortcut };
            staged.push(key);
            key
        };
        next.push(RegisteredFeature { action, registered });
    }
    let obsolete = current
        .iter()
        .filter(|feature| {
            !next
                .iter()
                .any(|next| next.registered.id == feature.registered.id)
        })
        .copied()
        .collect::<Vec<_>>();
    let mut removed = Vec::<RegisteredHotkey>::new();
    for obsolete in obsolete {
        if let Err(error) = unsafe { UnregisterHotKey(Some(hwnd), obsolete.registered.id) } {
            let mut lost = HashSet::new();
            let mut restore_errors = Vec::new();
            for old in &removed {
                if let Err(error) = register(hwnd, old.id, old.shortcut) {
                    lost.insert(old.id);
                    restore_errors.push(format!("{}：{error}", old.shortcut.label()));
                }
            }
            for key in &staged {
                unsafe {
                    let _ = UnregisterHotKey(Some(hwnd), key.id);
                }
            }
            if !restore_errors.is_empty() {
                // Do not claim a lost OS registration is still available. A
                // later retry must register it again rather than reuse its ID.
                current.retain(|feature| !lost.contains(&feature.registered.id));
                bail!(
                    "无法释放旧快捷键，且部分快捷键无法恢复：{}；原始错误：{error}",
                    restore_errors.join("；")
                );
            }
            return Err(error).context("无法释放旧功能快捷键，已恢复原设置");
        }
        removed.push(obsolete.registered);
    }
    *current = next;
    Ok(())
}

fn register_shortcut(hwnd: HWND, preferred: Shortcut) -> Option<RegisteredHotkey> {
    let default = Shortcut::parse("Alt+Space").expect("valid built-in shortcut");
    let mut candidates = vec![preferred];
    if preferred == default {
        candidates.push(Shortcut::parse("Ctrl+Alt+Space").expect("valid built-in shortcut"));
    }
    candidates.into_iter().find_map(|shortcut| {
        register(hwnd, HOTKEY_ID, shortcut)
            .ok()
            .map(|()| RegisteredHotkey {
                id: HOTKEY_ID,
                shortcut,
            })
    })
}

fn register(hwnd: HWND, id: i32, shortcut: Shortcut) -> windows::core::Result<()> {
    unsafe {
        RegisterHotKey(
            Some(hwnd),
            id,
            HOT_KEY_MODIFIERS(shortcut.modifiers) | MOD_NOREPEAT,
            shortcut.key,
        )
    }
}

fn replace_shortcut(
    hwnd: HWND,
    current: Option<RegisteredHotkey>,
    shortcut: Shortcut,
) -> Result<RegisteredHotkey> {
    if let Some(registered) = current
        && registered.shortcut == shortcut
    {
        return Ok(registered);
    }
    let id = if current.is_some_and(|registered| registered.id == HOTKEY_ID) {
        HOTKEY_ID + 1
    } else {
        HOTKEY_ID
    };
    register(hwnd, id, shortcut)
        .with_context(|| format!("{} 已被占用或无法注册，请选择其他组合", shortcut.label()))?;
    if let Some(registered) = current
        && let Err(error) = unsafe { UnregisterHotKey(Some(hwnd), registered.id) }
    {
        let _ = unsafe { UnregisterHotKey(Some(hwnd), id) };
        return Err(error).context("无法替换当前快捷键");
    }
    Ok(RegisteredHotkey { id, shortcut })
}

unsafe extern "system" fn window_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    unsafe {
        if msg == WM_NCCREATE {
            let created = &*(lparam.0 as *const CREATESTRUCTW);
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, created.lpCreateParams as isize);
            return DefWindowProcW(hwnd, msg, wparam, lparam);
        }
        let host = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const Host;
        let Some(host) = host.as_ref() else {
            return DefWindowProcW(hwnd, msg, wparam, lparam);
        };
        match msg {
            WM_HOTKEY
                if host
                    .hotkey
                    .get()
                    .is_some_and(|key| wparam.0 == key.id as usize) =>
            {
                host.emit(TrayEvent::Toggle)
            }
            WM_HOTKEY => {
                if let Some(feature) = host
                    .features
                    .borrow()
                    .iter()
                    .find(|feature| feature.registered.id as usize == wparam.0)
                {
                    host.emit(TrayEvent::ActivateFeature(feature.action));
                }
            }
            CHANGE_HOTKEY => host.change_hotkeys(hwnd),
            TRAY_CALLBACK => {
                if host.version_four.get() {
                    match lparam.0 as u16 as u32 {
                        NIN_SELECT | NIN_KEYSELECT => host.emit(TrayEvent::Show),
                        WM_CONTEXTMENU => host.menu(hwnd, wparam),
                        _ => {}
                    }
                } else {
                    match lparam.0 as u32 {
                        WM_LBUTTONUP => host.emit(TrayEvent::Show),
                        WM_RBUTTONUP => host.menu(hwnd, wparam),
                        _ => {}
                    }
                }
            }
            WM_CLOSE => {
                let _ = EndMenu();
                let _ = DestroyWindow(hwnd);
            }
            WM_DESTROY => {
                if let Some(registered) = host.hotkey.get() {
                    let _ = UnregisterHotKey(Some(hwnd), registered.id);
                }
                for feature in host.features.borrow_mut().drain(..) {
                    let _ = UnregisterHotKey(Some(hwnd), feature.registered.id);
                }
                for request in host
                    .requests
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .drain(..)
                {
                    match request {
                        HotkeyRequest::Activation { response, .. } => {
                            let _ = response.try_send(Err(anyhow::anyhow!("系统托盘已关闭")));
                        }
                        HotkeyRequest::Features { response, .. } => {
                            let _ = response.try_send(Err(anyhow::anyhow!("系统托盘已关闭")));
                        }
                    }
                }
                let _ = Shell_NotifyIconW(NIM_DELETE, &host.icon_data(hwnd));
                PostQuitMessage(0);
            }
            WM_NCDESTROY => {
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
                return DefWindowProcW(hwnd, msg, wparam, lparam);
            }
            msg if msg != 0 && msg == host.taskbar_created => {
                host.show_icon(hwnd);
            }
            _ => return DefWindowProcW(hwnd, msg, wparam, lparam),
        }
        LRESULT(0)
    }
}

struct TrayIcon {
    handle: HICON,
    owned: bool,
}

impl TrayIcon {
    fn new() -> Result<Self> {
        if let Ok(handle) = create_mark() {
            return Ok(Self {
                handle,
                owned: true,
            });
        }
        let handle = unsafe { LoadIconW(None, IDI_APPLICATION) }.context("无法加载托盘图标")?;
        Ok(Self {
            handle,
            owned: false,
        })
    }
}

impl Drop for TrayIcon {
    fn drop(&mut self) {
        if self.owned {
            unsafe {
                let _ = DestroyIcon(self.handle);
            }
        }
    }
}

fn create_mark() -> windows::core::Result<HICON> {
    const SIZE: usize = 32;
    const LIME: u32 = 0xffdaf48a;
    const INK: u32 = 0xff102e29;
    let mut pixels = [0u32; SIZE * SIZE];
    let mut mask = [0u8; SIZE * (SIZE / 8)];
    for y in 0..SIZE {
        for x in 0..SIZE {
            let corner_x = 6 - (x.min(SIZE - 1 - x).min(6) as i32);
            let corner_y = 6 - (y.min(SIZE - 1 - y).min(6) as i32);
            if corner_x * corner_x + corner_y * corner_y > 36 {
                mask[y * (SIZE / 8) + x / 8] |= 0x80 >> (x % 8);
                continue;
            }
            let bar = ((7..11).contains(&x) && (11..22).contains(&y))
                || ((14..18).contains(&x) && (7..25).contains(&y))
                || ((21..25).contains(&x) && (10..23).contains(&y));
            pixels[y * SIZE + x] = if bar { INK } else { LIME };
        }
    }
    let bitmap_info = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: SIZE as i32,
            biHeight: -(SIZE as i32),
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        },
        ..Default::default()
    };
    unsafe {
        let mut bits = null_mut();
        let color = CreateDIBSection(None, &bitmap_info, DIB_RGB_COLORS, &mut bits, None, 0)?;
        ptr::copy_nonoverlapping(pixels.as_ptr(), bits.cast(), pixels.len());
        let mask = CreateBitmap(SIZE as i32, SIZE as i32, 1, 1, Some(mask.as_ptr().cast()));
        let icon = if mask.is_invalid() {
            Err(windows::core::Error::from_win32())
        } else {
            let icon = CreateIconIndirect(&ICONINFO {
                fIcon: true.into(),
                hbmColor: color,
                hbmMask: mask,
                ..Default::default()
            });
            let _ = DeleteObject(mask.into());
            icon
        };
        let _ = DeleteObject(color.into());
        icon
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn feature_keys_can_swap_and_conflicts_roll_back_all_registrations() -> Result<()> {
        let hwnd = HWND::default();
        let candidates = (13..=24)
            .map(|key| Shortcut::parse(&format!("Ctrl+Alt+Shift+F{key}")).unwrap())
            .collect::<Vec<_>>();
        let mut registered = Vec::new();
        for pair in candidates.windows(2) {
            if replace_feature_shortcuts(
                hwnd,
                &mut registered,
                &[
                    (FeatureAction::Apps, pair[0]),
                    (FeatureAction::Files, pair[1]),
                ],
                None,
            )
            .is_ok()
            {
                break;
            }
        }
        anyhow::ensure!(registered.len() == 2, "找不到可用的功能测试快捷键");
        let before = registered.clone();
        replace_feature_shortcuts(
            hwnd,
            &mut registered,
            &[
                (FeatureAction::Apps, before[1].registered.shortcut),
                (FeatureAction::Files, before[0].registered.shortcut),
            ],
            None,
        )?;
        assert_eq!(registered[0].registered, before[1].registered);
        assert_eq!(registered[1].registered, before[0].registered);
        let swapped = registered.clone();
        assert!(
            replace_feature_shortcuts(
                hwnd,
                &mut registered,
                &[(FeatureAction::Apps, swapped[0].registered.shortcut)],
                Some(swapped[0].registered.shortcut)
            )
            .is_err()
        );
        assert_eq!(registered, swapped);

        let occupied = registered
            .iter()
            .map(|feature| feature.registered.shortcut)
            .collect::<Vec<_>>();
        let worker_candidates = candidates.clone();
        let worker_occupied = occupied.clone();
        let (ready, waiting) = std::sync::mpsc::sync_channel(1);
        let (check, checked) = std::sync::mpsc::sync_channel::<Shortcut>(1);
        let (report, reported) = std::sync::mpsc::sync_channel(1);
        let worker = std::thread::spawn(move || {
            let blocked = worker_candidates.iter().copied().find(|key| {
                !worker_occupied.contains(key) && register(HWND::default(), 900, *key).is_ok()
            });
            let _ = ready.send(blocked);
            if blocked.is_some() {
                if let Ok(staged) = checked.recv() {
                    let old_preserved = worker_occupied.iter().enumerate().all(|(index, key)| {
                        let id = 910 + index as i32;
                        if register(HWND::default(), id, *key).is_ok() {
                            unsafe {
                                let _ = UnregisterHotKey(None, id);
                            }
                            false
                        } else {
                            true
                        }
                    });
                    let staged_released = register(HWND::default(), 920, staged).is_ok();
                    if staged_released {
                        unsafe {
                            let _ = UnregisterHotKey(None, 920);
                        }
                    }
                    let _ = report.send((old_preserved, staged_released));
                }
                unsafe {
                    let _ = UnregisterHotKey(None, 900);
                }
            }
        });
        let blocked = waiting.recv()?.context("找不到可用于冲突测试的快捷键")?;
        let free = candidates
            .iter()
            .copied()
            .find(|key| {
                if occupied.contains(key) || *key == blocked {
                    return false;
                }
                if register(hwnd, 950, *key).is_ok() {
                    unsafe {
                        let _ = UnregisterHotKey(None, 950);
                    }
                    true
                } else {
                    false
                }
            })
            .context("找不到可用于事务测试的快捷键")?;
        let conflict = replace_feature_shortcuts(
            hwnd,
            &mut registered,
            &[(FeatureAction::Apps, free), (FeatureAction::Files, blocked)],
            None,
        );
        check.send(free)?;
        let (old_preserved, staged_released) = reported.recv()?;
        worker.join().map_err(|_| anyhow::anyhow!("测试线程退出"))?;
        assert!(conflict.is_err());
        assert_eq!(registered, swapped);
        assert!(
            old_preserved,
            "failed transaction released an existing feature key"
        );
        assert!(
            staged_released,
            "failed transaction leaked a newly staged feature key"
        );
        replace_feature_shortcuts(hwnd, &mut registered, &[], None)?;
        assert!(registered.is_empty());
        Ok(())
    }

    #[test]
    fn conflicting_change_keeps_previous_shortcut_registered() -> Result<()> {
        let hwnd = HWND::default();
        let candidates = [
            "Ctrl+Alt+Shift+F6",
            "Ctrl+Alt+Shift+F7",
            "Ctrl+Alt+Shift+F8",
            "Ctrl+Alt+Shift+F9",
        ];
        let current = candidates
            .iter()
            .find_map(|text| {
                let shortcut = Shortcut::parse(text).unwrap();
                register(hwnd, HOTKEY_ID, shortcut)
                    .ok()
                    .map(|()| RegisteredHotkey {
                        id: HOTKEY_ID,
                        shortcut,
                    })
            })
            .context("找不到可用的测试快捷键")?;
        let (ready, waiting) = std::sync::mpsc::sync_channel(1);
        let (release, released) = std::sync::mpsc::sync_channel(1);
        let worker = std::thread::spawn(move || {
            let blocked = candidates.iter().find_map(|text| {
                let shortcut = Shortcut::parse(text).unwrap();
                if shortcut != current.shortcut && register(HWND::default(), 50, shortcut).is_ok() {
                    Some(shortcut)
                } else {
                    None
                }
            });
            let _ = ready.send(blocked);
            let _ = released.recv();
            if blocked.is_some() {
                let _ = unsafe { UnregisterHotKey(None, 50) };
            }
        });
        let blocked = waiting.recv()?.context("找不到第二个测试快捷键")?;
        let result = replace_shortcut(hwnd, Some(current), blocked);
        // The conflict returns before the previous registration is removed.
        let previous_still_present = unsafe { UnregisterHotKey(None, current.id) }.is_ok();
        release.send(())?;
        worker.join().map_err(|_| anyhow::anyhow!("测试线程退出"))?;
        assert!(result.is_err());
        assert!(previous_still_present);

        let replacement = replace_shortcut(hwnd, None, blocked)?;
        assert_eq!(replacement.shortcut, blocked);
        unsafe { UnregisterHotKey(None, replacement.id)? };
        Ok(())
    }
}
