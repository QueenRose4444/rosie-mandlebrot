//! Checks the tray icon end to end without touching the mouse, the keyboard or
//! window focus: menu commands are posted to the tray's hidden window as
//! `WM_COMMAND`, exactly what a click on a menu item sends.
//!
//! `cargo run -p rosie_mandlebrot_serve --example tray_check`

#[cfg(not(windows))]
fn main() {
    println!("the tray is Windows-only; nothing to check");
}

#[cfg(windows)]
fn main() -> anyhow::Result<()> {
    use anyhow::ensure;
    use rosie_mandlebrot_serve::desktop::{fullscreen_busy, hide_console, launched_by_double_click, Tray, TrayCmd, TrayState, TRAY_ICON_ID, TRAY_WINDOW_CLASS};
    use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};
    use windows_sys::Win32::Foundation::HWND;
    use windows_sys::Win32::UI::Shell::{Shell_NotifyIconW, NIM_MODIFY, NOTIFYICONDATAW};
    use windows_sys::Win32::UI::WindowsAndMessaging::{FindWindowW, PostMessageW, WM_COMMAND};

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }
    fn find_tray() -> isize {
        let class = wide(TRAY_WINDOW_CLASS);
        // SAFETY: the class string outlives the call; a null title matches any.
        unsafe { FindWindowW(class.as_ptr(), std::ptr::null()) as isize }
    }
    /// A no-op NIM_MODIFY succeeds only while the icon is in the notification area.
    fn icon_present(hwnd: isize) -> bool {
        let n = NOTIFYICONDATAW { cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32, hWnd: hwnd as HWND, uID: TRAY_ICON_ID, ..Default::default() };
        // SAFETY: `n` is fully initialised; no flags means nothing changes.
        unsafe { Shell_NotifyIconW(NIM_MODIFY, &n) != 0 }
    }
    fn post(hwnd: isize, cmd: TrayCmd) -> bool {
        // SAFETY: a plain post to a window handle; a stale one only fails.
        unsafe { PostMessageW(hwnd as HWND, WM_COMMAND, cmd.menu_id() as usize, 0) != 0 }
    }

    // A copy of this program on a console of its own, as Explorer gives it.
    if std::env::args().nth(1).as_deref() == Some("--console-probe") {
        let before = launched_by_double_click();
        hide_console();
        println!("{before} {}", launched_by_double_click());
        return Ok(());
    }

    println!("fullscreen_busy() = {}", fullscreen_busy());
    println!("launched_by_double_click() = {}", launched_by_double_click());
    {
        use std::os::windows::process::CommandExt;
        // CREATE_NO_WINDOW: the child gets a new console that it alone uses, with no window.
        let out = std::process::Command::new(std::env::current_exe()?).arg("--console-probe").creation_flags(0x0800_0000).output()?;
        let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
        println!("on a console of its own: launched_by_double_click() then after hide_console(): {text}");
        ensure!(text == "true false", "console probe said {text:?}");
    }

    // 1. Menu commands arrive, Quit ends the loop and removes the icon.
    let paused = Arc::new(AtomicBool::new(false));
    let reads = Arc::new(AtomicU32::new(0));
    let got = Arc::new(Mutex::new(Vec::new()));
    let tray = Tray::new("Rosie_Mandlebrot (tray check)")?;
    let hwnd = find_tray();
    ensure!(hwnd != 0, "no window of class {TRAY_WINDOW_CLASS}");
    println!("found the tray window {hwnd:#x} by class name; icon present: {}", icon_present(hwnd));
    let driver = std::thread::spawn(move || -> anyhow::Result<()> {
        for cmd in [TrayCmd::Pause, TrayCmd::Resume, TrayCmd::Open] {
            ensure!(post(hwnd, cmd), "PostMessageW {cmd:?} failed");
            std::thread::sleep(Duration::from_millis(300));
        }
        // Long enough for one status tick.
        std::thread::sleep(Duration::from_millis(2300));
        ensure!(post(hwnd, TrayCmd::Quit), "PostMessageW Quit failed");
        Ok(())
    });
    let started = Instant::now();
    {
        let (paused, reads, got) = (paused.clone(), reads.clone(), got.clone());
        let state = {
            let paused = paused.clone();
            move || {
                let n = reads.fetch_add(1, Ordering::SeqCst);
                TrayState { paused: paused.load(Ordering::SeqCst), status: format!("check, state read {n}") }
            }
        };
        tray.run(state, move |cmd| {
            println!("  command arrived: {cmd:?} after {:?}", started.elapsed());
            got.lock().unwrap().push(cmd);
            match cmd {
                TrayCmd::Pause => paused.store(true, Ordering::SeqCst),
                TrayCmd::Resume => paused.store(false, Ordering::SeqCst),
                TrayCmd::Open => println!("  (dummy open: would open http://127.0.0.1:8460)"),
                TrayCmd::Quit => {}
            }
        })?;
    }
    driver.join().expect("driver thread")?;
    let got = got.lock().unwrap().clone();
    let reads = reads.load(Ordering::SeqCst);
    println!("run returned after {:?}; commands {got:?}; state read {reads} times", started.elapsed());
    ensure!(got == [TrayCmd::Pause, TrayCmd::Resume, TrayCmd::Open, TrayCmd::Quit], "wrong commands {got:?}");
    // One read at start, one after each of Pause/Resume/Open, at least one timer tick.
    ensure!(reads >= 5, "the status was read only {reads} times");
    ensure!(!icon_present(hwnd), "the icon is still in the notification area");
    ensure!(find_tray() == 0, "the hidden window is still there");
    println!("after run: icon present: false, window gone: true");

    // 2. A handle from another thread ends the loop; on() does not see Quit.
    let tray = Tray::new("Rosie_Mandlebrot (tray check 2)")?;
    let handle = tray.handle();
    let hwnd2 = find_tray();
    ensure!(icon_present(hwnd2), "the second icon was not added");
    let quitter = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(500));
        let refreshed = handle.refresh();
        (refreshed, handle.quit())
    });
    let mut seen = Vec::new();
    tray.run(TrayState::default, |cmd| seen.push(cmd))?;
    let (refreshed, quit) = quitter.join().expect("quit thread");
    ensure!(refreshed && quit && seen.is_empty(), "handle: refresh {refreshed}, quit {quit}, commands {seen:?}");
    ensure!(!icon_present(hwnd2) && !handle.quit(), "the second tray did not go away");
    println!("TrayHandle::quit from another thread ended the loop; icon removed; a later quit() returns false");
    println!("tray check passed");
    Ok(())
}
