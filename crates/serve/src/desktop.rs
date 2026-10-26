//! Running as a desktop app on Windows: a notification-area (tray) icon with a
//! small menu, a check for a full-screen program (a game) in front, and
//! console detection for a program started by double-click from Explorer.
//! Other platforms get stubs: no tray, never busy, never double-clicked.
//!
//! The tray is a [`Tray`] made and run on one thread, which it then owns:
//!
//! ```no_run
//! # use rosie_mandlebrot_serve::desktop::{Tray, TrayCmd, TrayState};
//! let tray = Tray::new("Rosie_Mandlebrot")?;
//! let handle = tray.handle(); // Send + Copy: `handle.quit()` ends `run` from any thread
//! tray.run(
//!     || TrayState { paused: false, status: "idle".into() },
//!     |cmd| match cmd {
//!         TrayCmd::Open => { /* open the page */ }
//!         TrayCmd::Pause | TrayCmd::Resume => { /* flip the queue */ }
//!         TrayCmd::Quit => { /* shut the server down */ }
//!     },
//! )?;
//! # anyhow::Ok(())
//! ```
//!
//! `run` returns after Quit is chosen (once `on(TrayCmd::Quit)` has returned),
//! after [`TrayHandle::quit`], or after Windows ends the session; in every case
//! the icon is removed before it returns.

pub use imp::{fullscreen_busy, hide_console, launched_by_double_click, Tray, TrayHandle};

/// A command chosen from the tray icon.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayCmd {
    /// "Open Rosie_Mandlebrot", or a left click on the icon.
    Open,
    /// "Pause rendering" (shown while not paused).
    Pause,
    /// "Resume rendering" (shown while paused).
    Resume,
    /// "Quit": `run` returns once the callback has handled it.
    Quit,
}

impl TrayCmd {
    /// The menu item id that carries this command in `WM_COMMAND`.
    pub const fn menu_id(self) -> u16 {
        match self {
            TrayCmd::Open => 1,
            TrayCmd::Pause => 2,
            TrayCmd::Resume => 3,
            TrayCmd::Quit => 4,
        }
    }

    pub const fn from_menu_id(id: u16) -> Option<TrayCmd> {
        match id {
            1 => Some(TrayCmd::Open),
            2 => Some(TrayCmd::Pause),
            3 => Some(TrayCmd::Resume),
            4 => Some(TrayCmd::Quit),
            _ => None,
        }
    }
}

/// What the tray shows: which of Pause/Resume the menu offers, and a status
/// line under the name in the tooltip (empty = name only).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TrayState {
    pub paused: bool,
    pub status: String,
}

/// Class name of the tray's hidden window (one per process).
pub const TRAY_WINDOW_CLASS: &str = "RosieMandlebrotTray";
/// The icon's id under that window, as `Shell_NotifyIconW` knows it.
pub const TRAY_ICON_ID: u32 = 1;
/// How often the state callback is read for the tooltip.
pub const STATUS_INTERVAL_MS: u32 = 2000;
/// Side of the generated icon, pixels.
pub const ICON_SIZE: usize = 32;

/// Tooltip text as UTF-16 without the terminator: the name, then the status on
/// its own line, cut to the 127 units the tooltip holds (never mid-pair).
pub fn tip_utf16(name: &str, status: &str) -> Vec<u16> {
    let text = if status.is_empty() { name.to_string() } else { format!("{name}\n{status}") };
    let mut out: Vec<u16> = Vec::with_capacity(127);
    for c in text.chars() {
        let mut buf = [0u16; 2];
        let units = c.encode_utf16(&mut buf);
        if out.len() + units.len() > 127 {
            break;
        }
        out.extend_from_slice(units);
    }
    out
}

/// The icon, as on the exe and the web page: the Mandelbrot set, dark
/// (#14171c), on an orange (#ff8a2a) rounded square; 4×4 samples a pixel.
/// `size * size` pixels in 0xAARRGGBB with straight alpha, top row first.
pub fn icon_pixels(size: usize) -> Vec<u32> {
    const ORANGE: [f32; 3] = [255.0, 138.0, 42.0];
    const DARK: [f32; 3] = [20.0, 23.0, 28.0];
    const SS: usize = 4;
    let n = size as f64;
    let radius = 0.22 * n;
    // Inside the rounded square?
    let in_square = |x: f64, y: f64| {
        let cx = x.clamp(radius, n - radius);
        let cy = y.clamp(radius, n - radius);
        (x - cx).powi(2) + (y - cy).powi(2) <= radius * radius
    };
    // The whole set, centred on -0.75, 2.9 wide.
    let in_set = |x: f64, y: f64| {
        let (cr, ci) = (-0.75 + (x / n - 0.5) * 2.9, (y / n - 0.5) * 2.9);
        let (mut zr, mut zi) = (0.0f64, 0.0f64);
        for _ in 0..200 {
            let t = zr * zr - zi * zi + cr;
            zi = 2.0 * zr * zi + ci;
            zr = t;
            if zr * zr + zi * zi > 4.0 {
                return false;
            }
        }
        true
    };
    let mut px = Vec::with_capacity(size * size);
    for y in 0..size {
        for x in 0..size {
            let (mut cover, mut set) = (0, 0);
            for sy in 0..SS {
                for sx in 0..SS {
                    let (fx, fy) = (x as f64 + (sx as f64 + 0.5) / SS as f64, y as f64 + (sy as f64 + 0.5) / SS as f64);
                    if in_square(fx, fy) {
                        cover += 1;
                        set += usize::from(in_set(fx, fy));
                    }
                }
            }
            if cover == 0 {
                px.push(0);
                continue;
            }
            let t = set as f32 / cover as f32;
            let ch = |i: usize| (ORANGE[i] + (DARK[i] - ORANGE[i]) * t).round() as u32;
            let a = (cover * 255 / (SS * SS)) as u32;
            px.push((a << 24) | (ch(0) << 16) | (ch(1) << 8) | ch(2));
        }
    }
    px
}

#[cfg(windows)]
mod imp {
    use super::{icon_pixels, tip_utf16, TrayCmd, TrayState, ICON_SIZE, STATUS_INTERVAL_MS, TRAY_ICON_ID, TRAY_WINDOW_CLASS};
    use anyhow::{bail, Context};
    use std::any::Any;
    use std::cell::{Cell, RefCell};
    use std::panic::{catch_unwind, resume_unwind, AssertUnwindSafe};
    use std::ptr::{null, null_mut};
    use std::time::{Duration, Instant};
    use windows_sys::Win32::Foundation::{GetLastError, ERROR_CLASS_ALREADY_EXISTS, HWND, LPARAM, LRESULT, WPARAM};
    use windows_sys::Win32::Graphics::Gdi::{CreateBitmap, CreateDIBSection, DeleteObject, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS};
    use windows_sys::Win32::System::Console::{FreeConsole, GetConsoleProcessList};
    use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows_sys::Win32::UI::Shell::{
        SHQueryUserNotificationState, Shell_NotifyIconW, NIF_ICON, NIF_MESSAGE, NIF_SHOWTIP, NIF_TIP, NIM_ADD, NIM_DELETE, NIM_MODIFY, NIM_SETVERSION, NIN_SELECT, NOTIFYICONDATAW,
        NOTIFYICON_VERSION_4, NOTIFY_ICON_DATA_FLAGS, QUERY_USER_NOTIFICATION_STATE, QUNS_BUSY, QUNS_PRESENTATION_MODE, QUNS_RUNNING_D3D_FULL_SCREEN,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        AppendMenuW, ChangeWindowMessageFilterEx, CreateIconIndirect, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyIcon, DestroyMenu, DestroyWindow, DispatchMessageW, GetMessageW,
        GetWindowLongPtrW, KillTimer, PostMessageW, PostQuitMessage, RegisterClassW, RegisterWindowMessageW, SetForegroundWindow, SetMenuDefaultItem, SetTimer, SetWindowLongPtrW, TrackPopupMenu,
        TranslateMessage, GWLP_USERDATA, HICON, ICONINFO, MF_SEPARATOR, MF_STRING, MSG, MSGFLT_ALLOW, TPM_BOTTOMALIGN, TPM_RIGHTBUTTON, WM_APP, WM_CLOSE, WM_COMMAND, WM_CONTEXTMENU,
        WM_ENDSESSION, WM_NULL, WM_TIMER, WNDCLASSW, WS_EX_TOOLWINDOW, WS_POPUP,
    };

    /// The icon's callback message.
    const WM_TRAY: u32 = WM_APP + 1;
    /// Posted by [`TrayHandle::quit`].
    const WM_QUIT_REQ: u32 = WM_APP + 2;
    /// Posted by [`TrayHandle::refresh`].
    const WM_REFRESH: u32 = WM_APP + 3;
    /// Enter or Space on the focused icon (version-4 notifications).
    const NIN_KEYSELECT: u32 = NIN_SELECT | 1;
    const TIMER_ID: usize = 1;
    /// Clicks closer together than this open the page once.
    const OPEN_DEBOUNCE: Duration = Duration::from_millis(800);

    /// True while a full-screen program, a presentation or "busy" quiet mode
    /// is in front of the desktop.
    pub fn fullscreen_busy() -> bool {
        let mut state: QUERY_USER_NOTIFICATION_STATE = 0;
        // SAFETY: `state` is a valid out-pointer for the duration of the call.
        let hr = unsafe { SHQueryUserNotificationState(&mut state) };
        hr >= 0 && matches!(state, QUNS_BUSY | QUNS_RUNNING_D3D_FULL_SCREEN | QUNS_PRESENTATION_MODE)
    }

    /// True when this process is the only one on its console, which is the
    /// case when Explorer started it (a terminal is attached to it as well).
    pub fn launched_by_double_click() -> bool {
        let mut ids = [0u32; 4];
        // SAFETY: the buffer holds `ids.len()` entries; a larger count is only reported.
        let n = unsafe { GetConsoleProcessList(ids.as_mut_ptr(), ids.len() as u32) };
        n == 1
    }

    /// Detach from the console; a console nobody else uses closes with it.
    pub fn hide_console() {
        // SAFETY: no arguments; failure (no console) is harmless.
        unsafe { FreeConsole() };
    }

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    fn nid(hwnd: HWND, icon: HICON, flags: NOTIFY_ICON_DATA_FLAGS) -> NOTIFYICONDATAW {
        NOTIFYICONDATAW {
            cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
            hWnd: hwnd,
            uID: TRAY_ICON_ID,
            uFlags: flags,
            uCallbackMessage: WM_TRAY,
            hIcon: icon,
            ..Default::default()
        }
    }

    /// Put the icon in the notification area with version-4 notifications.
    fn add_icon(hwnd: HWND, icon: HICON, tip: &[u16]) -> bool {
        let mut n = nid(hwnd, icon, NIF_MESSAGE | NIF_ICON | NIF_TIP | NIF_SHOWTIP);
        n.szTip[..tip.len()].copy_from_slice(tip);
        // SAFETY: `n` is fully initialised and outlives the call.
        if unsafe { Shell_NotifyIconW(NIM_ADD, &n) } == 0 {
            return false;
        }
        n.Anonymous.uVersion = NOTIFYICON_VERSION_4;
        // SAFETY: as above.
        unsafe { Shell_NotifyIconW(NIM_SETVERSION, &n) };
        true
    }

    fn make_icon() -> anyhow::Result<HICON> {
        let size = ICON_SIZE as i32;
        let px = icon_pixels(ICON_SIZE);
        let info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: size,
                // Negative: rows run top to bottom, as `icon_pixels` makes them.
                biHeight: -size,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bits = null_mut();
        // SAFETY: `info` describes a 32-bpp DIB; a null DC is allowed with DIB_RGB_COLORS.
        let color = unsafe { CreateDIBSection(null_mut(), &info, DIB_RGB_COLORS, &mut bits, null_mut(), 0) };
        if color.is_null() || bits.is_null() {
            bail!("CreateDIBSection failed");
        }
        // SAFETY: the section holds ICON_SIZE² 32-bit pixels, exactly `px.len()`.
        unsafe { std::ptr::copy_nonoverlapping(px.as_ptr(), bits.cast::<u32>(), px.len()) };
        // All-zero AND mask: the colour bitmap's alpha decides what shows.
        let mask_bits = [0u8; ICON_SIZE * ICON_SIZE / 8];
        // SAFETY: `mask_bits` holds ICON_SIZE rows of ICON_SIZE bits (rows are already word-aligned).
        let mask = unsafe { CreateBitmap(size, size, 1, 1, mask_bits.as_ptr().cast()) };
        let ii = ICONINFO { fIcon: 1, xHotspot: 0, yHotspot: 0, hbmMask: mask, hbmColor: color };
        // SAFETY: both bitmaps are valid (a null mask makes this fail cleanly); the icon copies them.
        let icon = unsafe { CreateIconIndirect(&ii) };
        // SAFETY: the bitmaps are ours and no longer used.
        unsafe {
            DeleteObject(color);
            DeleteObject(mask);
        }
        if icon.is_null() {
            bail!("CreateIconIndirect failed");
        }
        Ok(icon)
    }

    /// A tray icon and its hidden window. Made on the thread that will call
    /// [`Tray::run`]; it cannot move to another thread.
    pub struct Tray {
        hwnd: HWND,
        icon: HICON,
        tooltip: String,
        taskbar_created: u32,
    }

    /// Ends or refreshes a running [`Tray`] from any thread.
    #[derive(Debug, Clone, Copy)]
    pub struct TrayHandle(isize);

    impl TrayHandle {
        /// Make `run` return (without calling `on(TrayCmd::Quit)`). False when
        /// the tray is already gone.
        pub fn quit(&self) -> bool {
            self.post(WM_QUIT_REQ)
        }

        /// Read the state callback now instead of at the next tick.
        pub fn refresh(&self) -> bool {
            self.post(WM_REFRESH)
        }

        fn post(&self, msg: u32) -> bool {
            // SAFETY: posting to a window handle that may be stale only fails.
            unsafe { PostMessageW(self.0 as HWND, msg, 0, 0) != 0 }
        }
    }

    impl Tray {
        /// Make the hidden window and add the icon. `tooltip` is the first
        /// tooltip line. If the taskbar is not up yet, the icon is added when
        /// it appears.
        pub fn new(tooltip: &str) -> anyhow::Result<Tray> {
            let class = wide(TRAY_WINDOW_CLASS);
            // SAFETY: a null name asks for this executable's module handle.
            let hinst = unsafe { GetModuleHandleW(null()) };
            let wc = WNDCLASSW { lpfnWndProc: Some(wndproc), hInstance: hinst, lpszClassName: class.as_ptr(), ..Default::default() };
            // SAFETY: `wc` and the class name it points at outlive the call.
            if unsafe { RegisterClassW(&wc) } == 0 {
                // SAFETY: reads this thread's last error, no arguments.
                if unsafe { GetLastError() } != ERROR_CLASS_ALREADY_EXISTS {
                    bail!("RegisterClassW failed");
                }
            }
            let title = wide("Rosie_Mandlebrot tray");
            // SAFETY: the strings outlive the call; a never-shown popup is a top-level window,
            // so it receives TaskbarCreated (a message-only window would not).
            let hwnd = unsafe { CreateWindowExW(WS_EX_TOOLWINDOW, class.as_ptr(), title.as_ptr(), WS_POPUP, 0, 0, 0, 0, null_mut(), null_mut(), hinst, null()) };
            if hwnd.is_null() {
                bail!("CreateWindowExW failed");
            }
            let icon = match make_icon() {
                Ok(i) => i,
                Err(e) => {
                    // SAFETY: the window was made above on this thread.
                    unsafe { DestroyWindow(hwnd) };
                    return Err(e);
                }
            };
            let tbc = wide("TaskbarCreated");
            // SAFETY: the string outlives the call.
            let taskbar_created = unsafe { RegisterWindowMessageW(tbc.as_ptr()) };
            // SAFETY: lets Explorer's broadcast through when this process runs elevated; no out-struct.
            unsafe { ChangeWindowMessageFilterEx(hwnd, taskbar_created, MSGFLT_ALLOW, null_mut()) };
            let tray = Tray { hwnd, icon, tooltip: tooltip.to_string(), taskbar_created };
            if !add_icon(hwnd, icon, &tip_utf16(tooltip, "")) {
                tracing::warn!("the tray icon could not be added yet; it will be once the taskbar is up");
            }
            Ok(tray)
        }

        pub fn handle(&self) -> TrayHandle {
            TrayHandle(self.hwnd as isize)
        }

        /// Run the message loop on this thread until Quit (see the module
        /// docs). `state` is read for the menu and every
        /// [`STATUS_INTERVAL_MS`] for the tooltip; `on` gets each command. A
        /// panic in either ends the loop, removes the icon and carries on
        /// unwinding from here.
        pub fn run(self, state: impl Fn() -> TrayState, mut on: impl FnMut(TrayCmd)) -> anyhow::Result<()> {
            let ctx = Ctx {
                hwnd: self.hwnd,
                icon: self.icon,
                tooltip: &self.tooltip,
                taskbar_created: self.taskbar_created,
                state: &state,
                on: RefCell::new(&mut on),
                last_tip: RefCell::new(Vec::new()),
                last_open: Cell::new(None),
                panic: Cell::new(None),
            };
            // SAFETY: `ctx` outlives the loop below, and the pointer is cleared before `ctx` is dropped.
            unsafe { SetWindowLongPtrW(self.hwnd, GWLP_USERDATA, &ctx as *const Ctx as isize) };
            ctx.update_tip();
            // SAFETY: a timer on our own window, killed below.
            unsafe { SetTimer(self.hwnd, TIMER_ID, STATUS_INTERVAL_MS, None) };
            let result = pump();
            // SAFETY: same window; after this the window procedure no longer sees `ctx`.
            unsafe {
                KillTimer(self.hwnd, TIMER_ID);
                SetWindowLongPtrW(self.hwnd, GWLP_USERDATA, 0);
            }
            let panic = ctx.panic.take();
            drop(ctx);
            drop(self);
            if let Some(p) = panic {
                resume_unwind(p);
            }
            result
        }
    }

    impl Drop for Tray {
        fn drop(&mut self) {
            let n = nid(self.hwnd, self.icon, 0);
            // SAFETY: handles made in `new` on this thread (`Tray` is not Send); deleting a
            // missing icon only fails.
            unsafe {
                Shell_NotifyIconW(NIM_DELETE, &n);
                DestroyWindow(self.hwnd);
                DestroyIcon(self.icon);
            }
        }
    }

    fn pump() -> anyhow::Result<()> {
        // SAFETY: MSG is plain data; all-zero is a valid value.
        let mut msg: MSG = unsafe { std::mem::zeroed() };
        loop {
            // SAFETY: `msg` is a valid out-pointer; a null window takes thread messages too (WM_QUIT).
            match unsafe { GetMessageW(&mut msg, null_mut(), 0, 0) } {
                0 => return Ok(()),
                -1 => return Err(std::io::Error::last_os_error()).context("GetMessageW failed"),
                _ => {
                    // SAFETY: `msg` was just filled in by GetMessageW.
                    unsafe {
                        TranslateMessage(&msg);
                        DispatchMessageW(&msg);
                    }
                }
            }
        }
    }

    fn quit_loop() {
        // SAFETY: posts WM_QUIT to this thread's queue, no pointers.
        unsafe { PostQuitMessage(0) };
    }

    /// What the window procedure needs while `run` is going. Shared by
    /// reference only: the menu's modal loop re-enters the procedure.
    struct Ctx<'a> {
        hwnd: HWND,
        icon: HICON,
        tooltip: &'a str,
        taskbar_created: u32,
        state: &'a dyn Fn() -> TrayState,
        on: RefCell<&'a mut dyn FnMut(TrayCmd)>,
        last_tip: RefCell<Vec<u16>>,
        last_open: Cell<Option<Instant>>,
        panic: Cell<Option<Box<dyn Any + Send>>>,
    }

    impl Ctx<'_> {
        fn handle(&self, msg: u32, wp: WPARAM, lp: LPARAM) -> Option<LRESULT> {
            match msg {
                WM_TRAY => {
                    // Version 4: the event is the low word of lParam, the anchor point is in wParam.
                    match (lp as u32) & 0xffff {
                        NIN_SELECT | NIN_KEYSELECT => self.clicked(),
                        WM_CONTEXTMENU => self.menu((wp & 0xffff) as u16 as i16 as i32, ((wp >> 16) & 0xffff) as u16 as i16 as i32),
                        _ => {}
                    }
                }
                WM_COMMAND => {
                    if let Some(cmd) = TrayCmd::from_menu_id((wp & 0xffff) as u16) {
                        self.command(cmd);
                    }
                }
                WM_TIMER if wp == TIMER_ID => self.update_tip(),
                WM_REFRESH => self.update_tip(),
                WM_QUIT_REQ | WM_CLOSE => quit_loop(),
                // The session is ending and the process goes when this returns.
                WM_ENDSESSION if wp != 0 => self.command(TrayCmd::Quit),
                m if m == self.taskbar_created => {
                    let tip = self.last_tip.borrow().clone();
                    let tip = if tip.is_empty() { tip_utf16(self.tooltip, "") } else { tip };
                    let n = nid(self.hwnd, self.icon, 0);
                    // SAFETY: `n` is fully initialised; deleting a missing icon only fails.
                    unsafe { Shell_NotifyIconW(NIM_DELETE, &n) };
                    add_icon(self.hwnd, self.icon, &tip);
                }
                _ => return None,
            }
            Some(0)
        }

        /// Run user code; a panic is kept for `run` to re-raise and ends the loop.
        fn guarded<T>(&self, f: impl FnOnce() -> T) -> Option<T> {
            match catch_unwind(AssertUnwindSafe(f)) {
                Ok(v) => Some(v),
                Err(p) => {
                    let first = self.panic.take().unwrap_or(p);
                    self.panic.set(Some(first));
                    quit_loop();
                    None
                }
            }
        }

        fn read_state(&self) -> Option<TrayState> {
            self.guarded(|| (self.state)())
        }

        fn command(&self, cmd: TrayCmd) {
            if let Ok(mut on) = self.on.try_borrow_mut() {
                self.guarded(|| (*on)(cmd));
            }
            if cmd == TrayCmd::Quit {
                quit_loop();
            } else {
                self.update_tip();
            }
        }

        fn clicked(&self) {
            let now = Instant::now();
            if self.last_open.get().is_some_and(|t| now.duration_since(t) < OPEN_DEBOUNCE) {
                return;
            }
            self.last_open.set(Some(now));
            self.command(TrayCmd::Open);
        }

        fn update_tip(&self) {
            let Some(s) = self.read_state() else { return };
            let tip = tip_utf16(self.tooltip, &s.status);
            if *self.last_tip.borrow() == tip {
                return;
            }
            let mut n = nid(self.hwnd, self.icon, NIF_TIP | NIF_SHOWTIP);
            n.szTip[..tip.len()].copy_from_slice(&tip);
            // SAFETY: `n` is fully initialised. Failure (no taskbar) is retried next tick.
            if unsafe { Shell_NotifyIconW(NIM_MODIFY, &n) } != 0 {
                *self.last_tip.borrow_mut() = tip;
            }
        }

        fn menu(&self, x: i32, y: i32) {
            let paused = self.read_state().is_some_and(|s| s.paused);
            // SAFETY: no arguments; checked for null below.
            let menu = unsafe { CreatePopupMenu() };
            if menu.is_null() {
                return;
            }
            let toggle = if paused { (TrayCmd::Resume, "Resume rendering") } else { (TrayCmd::Pause, "Pause rendering") };
            let items = [(Some(TrayCmd::Open), "Open Rosie_Mandlebrot"), (Some(toggle.0), toggle.1), (None, ""), (Some(TrayCmd::Quit), "Quit")];
            for (cmd, label) in items {
                let text = wide(label);
                let (flags, id, text) = match cmd {
                    Some(c) => (MF_STRING, c.menu_id() as usize, text.as_ptr()),
                    None => (MF_SEPARATOR, 0, null()),
                };
                // SAFETY: `menu` is ours; the string is copied by the call.
                unsafe { AppendMenuW(menu, flags, id, text) };
            }
            // SAFETY: `menu` is ours; the menu's commands come back as WM_COMMAND to `hwnd`.
            // SetForegroundWindow first and WM_NULL after are what make the menu close
            // when the user clicks elsewhere (documented TrackPopupMenu behaviour).
            unsafe {
                SetMenuDefaultItem(menu, TrayCmd::Open.menu_id() as u32, 0);
                SetForegroundWindow(self.hwnd);
                TrackPopupMenu(menu, TPM_RIGHTBUTTON | TPM_BOTTOMALIGN, x, y, 0, self.hwnd, null());
                PostMessageW(self.hwnd, WM_NULL, 0, 0);
                DestroyMenu(menu);
            }
        }
    }

    unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
        // SAFETY: the user data is 0 or the live `Ctx` set by `Tray::run`, which clears it
        // before the `Ctx` is dropped; it is only used for the length of this call.
        let ctx = unsafe { (GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const Ctx<'static>).as_ref() };
        if let Some(r) = ctx.and_then(|c| c.handle(msg, wp, lp)) {
            return r;
        }
        if msg == WM_CLOSE {
            // Only `Tray`'s drop destroys the window.
            return 0;
        }
        // SAFETY: forwards this window's own message unchanged.
        unsafe { DefWindowProcW(hwnd, msg, wp, lp) }
    }
}

#[cfg(not(windows))]
mod imp {
    use super::{TrayCmd, TrayState};

    pub fn fullscreen_busy() -> bool {
        false
    }

    pub fn launched_by_double_click() -> bool {
        false
    }

    pub fn hide_console() {}

    /// No tray outside Windows: [`Tray::new`] always fails.
    pub struct Tray {
        _private: (),
    }

    #[derive(Debug, Clone, Copy)]
    pub struct TrayHandle;

    impl TrayHandle {
        pub fn quit(&self) -> bool {
            false
        }

        pub fn refresh(&self) -> bool {
            false
        }
    }

    impl Tray {
        pub fn new(_tooltip: &str) -> anyhow::Result<Tray> {
            anyhow::bail!("the tray icon is only available on Windows")
        }

        pub fn handle(&self) -> TrayHandle {
            TrayHandle
        }

        pub fn run(self, _state: impl Fn() -> TrayState, _on: impl FnMut(TrayCmd)) -> anyhow::Result<()> {
            Ok(())
        }
    }
}

/// [`Tray::new`] then [`Tray::run`], for callers that end the tray only from its menu.
pub fn run_tray(tooltip: &str, state: impl Fn() -> TrayState, on: impl FnMut(TrayCmd)) -> anyhow::Result<()> {
    Tray::new(tooltip)?.run(state, on)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn menu_ids_round_trip() {
        for c in [TrayCmd::Open, TrayCmd::Pause, TrayCmd::Resume, TrayCmd::Quit] {
            assert_eq!(TrayCmd::from_menu_id(c.menu_id()), Some(c));
        }
        assert_eq!(TrayCmd::from_menu_id(0), None);
        assert_eq!(TrayCmd::from_menu_id(5), None);
    }

    #[test]
    fn tooltip_lines_and_limit() {
        let t = |n: &str, s: &str| String::from_utf16(&tip_utf16(n, s)).unwrap();
        assert_eq!(t("Rosie_Mandlebrot", ""), "Rosie_Mandlebrot");
        assert_eq!(t("Rosie_Mandlebrot", "rendering seahorse 42%"), "Rosie_Mandlebrot\nrendering seahorse 42%");
        assert_eq!(tip_utf16("a", &"x".repeat(500)).len(), 127);
        // A pair that would straddle the limit is dropped whole.
        let tip = tip_utf16(&"y".repeat(126), "");
        assert_eq!(tip.len(), 126);
        let tip = tip_utf16(&format!("{}\u{1F600}", "y".repeat(126)), "");
        assert_eq!(tip.len(), 126);
        assert!(String::from_utf16(&tip).is_ok());
    }

    #[test]
    fn icon_is_the_set_dark_on_an_orange_rounded_square() {
        let s = ICON_SIZE;
        let px = icon_pixels(s);
        assert_eq!(px.len(), s * s);
        let at = |x: usize, y: usize| px[y * s + x];
        assert_eq!(at(0, 0), 0, "the rounded corners are transparent");
        assert_eq!(at(s - 1, s - 1), 0);
        // Pixel 22 across is -0.2 on the real axis: inside the main cardioid.
        assert_eq!(at(22, s / 2), 0xff14_171c, "the set is dark and opaque");
        assert_eq!(at(8, 3), 0xffff_8a2a, "outside the set is #ff8a2a");
        assert_eq!(at(0, s / 2) >> 24, 255, "the square's edge is opaque away from the corners");
        let curve = (0..8).flat_map(|y| (0..8).map(move |x| (x, y))).filter(|&(x, y)| (1..255).contains(&(at(x, y) >> 24))).count();
        assert!(curve > 2, "the corners' curve is antialiased ({curve} partly clear pixels)");
        assert!(px.iter().any(|&p| p >> 24 == 255 && p != 0xff14_171c && p != 0xffff_8a2a), "the set's edge is antialiased");
    }

    #[test]
    fn platform_queries_do_not_panic() {
        let _ = fullscreen_busy();
        let _ = launched_by_double_click();
    }
}
