#[cfg(target_os = "linux")]
mod linux_input {
    use std::os::raw::{c_int, c_uchar, c_uint, c_ulong, c_void};

    pub type Display = c_void;

    extern "C" {
        fn dlopen(filename: *const i8, flag: i32) -> *mut c_void;
        fn dlsym(handle: *mut c_void, symbol: *const i8) -> *mut c_void;
        fn dlclose(handle: *mut c_void) -> i32;
    }

    const RTLD_LAZY: i32 = 1;

    pub struct X11Api {
        lib_x11: *mut c_void,
        lib_xtst: *mut c_void,

        pub XOpenDisplay: unsafe extern "C" fn(display_name: *const i8) -> *mut Display,
        pub XCloseDisplay: unsafe extern "C" fn(display: *mut Display),
        pub XFlush: unsafe extern "C" fn(display: *mut Display),
        pub XKeysymToKeycode:
            unsafe extern "C" fn(display: *mut Display, keysym: c_ulong) -> c_uchar,

        pub XTestFakeMotionEvent: unsafe extern "C" fn(
            display: *mut Display,
            screen: c_int,
            x: c_int,
            y: c_int,
            delay: c_ulong,
        ) -> c_int,
        pub XTestFakeButtonEvent: unsafe extern "C" fn(
            display: *mut Display,
            button: c_int,
            is_press: c_int,
            delay: c_ulong,
        ) -> c_int,
        pub XTestFakeKeyEvent: unsafe extern "C" fn(
            display: *mut Display,
            keycode: c_uint,
            is_press: c_int,
            delay: c_ulong,
        ) -> c_int,
    }

    impl X11Api {
        pub fn load() -> Option<Self> {
            unsafe {
                let lib_x11 = dlopen(b"libX11.so.6\0".as_ptr() as *const i8, RTLD_LAZY);
                if lib_x11.is_null() {
                    return None;
                }
                let lib_xtst = dlopen(b"libXtst.so.6\0".as_ptr() as *const i8, RTLD_LAZY);
                if lib_xtst.is_null() {
                    dlclose(lib_x11);
                    return None;
                }

                macro_rules! load_sym {
                    ($lib:expr, $sym:expr, $ty:ty) => {{
                        let ptr = dlsym($lib, $sym.as_ptr() as *const i8);
                        if ptr.is_null() {
                            dlclose(lib_x11);
                            dlclose(lib_xtst);
                            return None;
                        }
                        std::mem::transmute::<*mut c_void, $ty>(ptr)
                    }};
                }

                let x_open_display = load_sym!(
                    lib_x11,
                    b"XOpenDisplay\0",
                    unsafe extern "C" fn(*const i8) -> *mut Display
                );
                let x_close_display = load_sym!(
                    lib_x11,
                    b"XCloseDisplay\0",
                    unsafe extern "C" fn(*mut Display)
                );
                let x_flush = load_sym!(lib_x11, b"XFlush\0", unsafe extern "C" fn(*mut Display));
                let x_keysym_to_keycode = load_sym!(
                    lib_x11,
                    b"XKeysymToKeycode\0",
                    unsafe extern "C" fn(*mut Display, c_ulong) -> c_uchar
                );

                let x_test_fake_motion_event = load_sym!(
                    lib_xtst,
                    b"XTestFakeMotionEvent\0",
                    unsafe extern "C" fn(*mut Display, c_int, c_int, c_int, c_ulong) -> c_int
                );
                let x_test_fake_button_event = load_sym!(
                    lib_xtst,
                    b"XTestFakeButtonEvent\0",
                    unsafe extern "C" fn(*mut Display, c_int, c_int, c_ulong) -> c_int
                );
                let x_test_fake_key_event = load_sym!(
                    lib_xtst,
                    b"XTestFakeKeyEvent\0",
                    unsafe extern "C" fn(*mut Display, c_uint, c_int, c_ulong) -> c_int
                );

                Some(X11Api {
                    lib_x11,
                    lib_xtst,
                    XOpenDisplay: x_open_display,
                    XCloseDisplay: x_close_display,
                    XFlush: x_flush,
                    XKeysymToKeycode: x_keysym_to_keycode,
                    XTestFakeMotionEvent: x_test_fake_motion_event,
                    XTestFakeButtonEvent: x_test_fake_button_event,
                    XTestFakeKeyEvent: x_test_fake_key_event,
                })
            }
        }
    }

    impl Drop for X11Api {
        fn drop(&mut self) {
            unsafe {
                dlclose(self.lib_x11);
                dlclose(self.lib_xtst);
            }
        }
    }
}

#[cfg(target_os = "windows")]
mod win32_input {
    use std::os::raw::{c_int, c_long};

    #[repr(C)]
    #[derive(Copy, Clone)]
    pub struct MOUSEINPUT {
        pub dx: c_long,
        pub dy: c_long,
        pub mouseData: u32,
        pub dwFlags: u32,
        pub time: u32,
        pub dwExtraInfo: usize,
    }

    #[repr(C)]
    #[derive(Copy, Clone)]
    pub struct KEYBDINPUT {
        pub wVk: u16,
        pub wScan: u16,
        pub dwFlags: u32,
        pub time: u32,
        pub dwExtraInfo: usize,
    }

    #[repr(C)]
    #[derive(Copy, Clone)]
    pub struct HARDWAREINPUT {
        pub uMsg: u32,
        pub wParamL: u16,
        pub wParamH: u16,
    }

    #[repr(C)]
    #[derive(Copy, Clone)]
    pub union INPUT_UNION {
        pub mi: MOUSEINPUT,
        pub ki: KEYBDINPUT,
        pub hi: HARDWAREINPUT,
    }

    #[repr(C)]
    #[derive(Copy, Clone)]
    pub struct INPUT {
        pub type_: u32,
        pub u: INPUT_UNION,
    }

    pub const INPUT_MOUSE: u32 = 0;
    pub const INPUT_KEYBOARD: u32 = 1;
    pub const KEYEVENTF_KEYUP: u32 = 0x0002;
    pub const MOUSEEVENTF_MOVE: u32 = 0x0001;
    pub const MOUSEEVENTF_LEFTDOWN: u32 = 0x0002;
    pub const MOUSEEVENTF_LEFTUP: u32 = 0x0004;
    pub const MOUSEEVENTF_RIGHTDOWN: u32 = 0x0008;
    pub const MOUSEEVENTF_RIGHTUP: u32 = 0x0010;
    pub const MOUSEEVENTF_MIDDLEDOWN: u32 = 0x0020;
    pub const MOUSEEVENTF_MIDDLEUP: u32 = 0x0040;
    pub const MOUSEEVENTF_WHEEL: u32 = 0x0800;
    pub const MOUSEEVENTF_ABSOLUTE: u32 = 0x8000;

    pub const SM_CXSCREEN: c_int = 0;
    pub const SM_CYSCREEN: c_int = 1;

    extern "system" {
        pub fn SendInput(cInputs: u32, pInputs: *const INPUT, cbSize: c_int) -> u32;
        pub fn GetSystemMetrics(nIndex: c_int) -> c_int;
    }
}

#[cfg(target_os = "macos")]
mod macos_input {
    use std::os::raw::{c_double, c_void};

    pub type CGEventRef = *mut c_void;
    pub type CGEventSourceRef = *mut c_void;

    #[repr(C)]
    #[derive(Copy, Clone)]
    pub struct CGPoint {
        pub x: c_double,
        pub y: c_double,
    }

    #[repr(transparent)]
    pub struct CGEventTapLocation(u32);
    pub const CGHIDEventTap: CGEventTapLocation = CGEventTapLocation(0);

    #[repr(transparent)]
    #[derive(Copy, Clone)]
    pub struct CGEventType(u32);
    pub const CGEventLeftMouseDown: CGEventType = CGEventType(1);
    pub const CGEventLeftMouseUp: CGEventType = CGEventType(2);
    pub const CGEventRightMouseDown: CGEventType = CGEventType(3);
    pub const CGEventRightMouseUp: CGEventType = CGEventType(4);
    pub const CGEventMouseMoved: CGEventType = CGEventType(5);
    pub const CGEventScrollWheel: CGEventType = CGEventType(22);
    pub const CGEventOtherMouseDown: CGEventType = CGEventType(25);
    pub const CGEventOtherMouseUp: CGEventType = CGEventType(26);

    #[repr(transparent)]
    pub struct CGMouseButton(u32);
    pub const CGMouseButtonLeft: CGMouseButton = CGMouseButton(0);
    pub const CGMouseButtonRight: CGMouseButton = CGMouseButton(1);
    pub const CGMouseButtonCenter: CGMouseButton = CGMouseButton(2);

    #[repr(transparent)]
    pub struct CGScrollEventUnit(u32);
    pub const kCGScrollEventUnitLine: CGScrollEventUnit = CGScrollEventUnit(0);

    #[link(name = "CoreFoundation", kind = "framework")]
    extern "C" {
        pub static kCFTypeDictionaryKeyCallBacks: c_void;
        pub static kCFTypeDictionaryValueCallBacks: c_void;
        pub static kCFBooleanTrue: *const c_void;
        pub fn CFDictionaryCreate(
            allocator: *const c_void,
            keys: *const *const c_void,
            values: *const *const c_void,
            numValues: isize,
            keyCallBacks: *const c_void,
            valueCallBacks: *const c_void,
        ) -> *const c_void;
    }

    #[link(name = "ApplicationServices", kind = "framework")]
    extern "C" {
        pub fn CGEventCreate(source: CGEventSourceRef) -> CGEventRef;
        pub fn CGEventGetLocation(event: CGEventRef) -> CGPoint;
        pub fn CGEventCreateMouseEvent(
            source: CGEventSourceRef,
            mouseType: CGEventType,
            mouseCursorPosition: CGPoint,
            mouseButton: CGMouseButton,
        ) -> CGEventRef;
        pub fn CGEventCreateKeyboardEvent(
            source: CGEventSourceRef,
            virtualKey: u16,
            keyDown: bool,
        ) -> CGEventRef;
        pub fn CGEventCreateScrollWheelEvent2(
            source: CGEventSourceRef,
            units: CGScrollEventUnit,
            wheelCount: u32,
            wheel1: i32,
            wheel2: i32,
            wheel3: i32,
        ) -> CGEventRef;
        pub fn CGEventPost(tap: CGEventTapLocation, event: CGEventRef);
        pub fn CFRelease(cf: *mut c_void);
        pub static kAXTrustedCheckOptionPrompt: *const c_void;
        pub fn AXIsProcessTrustedWithOptions(options: *const c_void) -> bool;
        pub fn AXIsProcessTrusted() -> bool;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn accessibility_permissions_returns_true_on_non_macos() {
        assert!(check_and_request_accessibility_permissions());
    }
}

/// Dynamic mouse control event simulator for Linux, Windows, and macOS.
pub fn simulate_mouse_input(event: &nodeinnet_p2p::p2p::DesktopInputEvent) {
    #[cfg(target_os = "linux")]
    {
        // Try native X11 / XTest emulation first by dynamically loading libX11 / libXtst
        if let Some(api) = linux_input::X11Api::load() {
            unsafe {
                let display = (api.XOpenDisplay)(std::ptr::null());
                if !display.is_null() {
                    match event {
                        nodeinnet_p2p::p2p::DesktopInputEvent::MouseMove { x, y } => {
                            (api.XTestFakeMotionEvent)(display, -1, *x, *y, 0);
                        }
                        nodeinnet_p2p::p2p::DesktopInputEvent::MouseDown { button } => {
                            (api.XTestFakeButtonEvent)(display, *button as i32, 1, 0);
                        }
                        nodeinnet_p2p::p2p::DesktopInputEvent::MouseUp { button } => {
                            (api.XTestFakeButtonEvent)(display, *button as i32, 0, 0);
                        }
                        nodeinnet_p2p::p2p::DesktopInputEvent::MouseScroll { dy } => {
                            let button = if *dy > 0 { 4 } else { 5 };
                            let clicks = dy.abs();
                            for _ in 0..clicks {
                                (api.XTestFakeButtonEvent)(display, button, 1, 0);
                                (api.XTestFakeButtonEvent)(display, button, 0, 0);
                            }
                        }
                        nodeinnet_p2p::p2p::DesktopInputEvent::KeyPress { keyval } => {
                            let keycode =
                                (api.XKeysymToKeycode)(display, *keyval as std::os::raw::c_ulong);
                            if keycode != 0 {
                                (api.XTestFakeKeyEvent)(
                                    display,
                                    keycode as std::os::raw::c_uint,
                                    1,
                                    0,
                                );
                            }
                        }
                        nodeinnet_p2p::p2p::DesktopInputEvent::KeyRelease { keyval } => {
                            let keycode =
                                (api.XKeysymToKeycode)(display, *keyval as std::os::raw::c_ulong);
                            if keycode != 0 {
                                (api.XTestFakeKeyEvent)(
                                    display,
                                    keycode as std::os::raw::c_uint,
                                    0,
                                    0,
                                );
                            }
                        }
                    }
                    (api.XFlush)(display);
                    (api.XCloseDisplay)(display);
                    return;
                }
            }
        }

        // Wayland / Headless safe CLI fallback (xdotool)
        match event {
            nodeinnet_p2p::p2p::DesktopInputEvent::MouseMove { x, y } => {
                let _ = std::process::Command::new("xdotool")
                    .args(&["mousemove", &x.to_string(), &y.to_string()])
                    .status();
            }
            nodeinnet_p2p::p2p::DesktopInputEvent::MouseDown { button } => {
                let btn_str = button.to_string();
                let _ = std::process::Command::new("xdotool")
                    .args(&["mousedown", &btn_str])
                    .status();
            }
            nodeinnet_p2p::p2p::DesktopInputEvent::MouseUp { button } => {
                let btn_str = button.to_string();
                let _ = std::process::Command::new("xdotool")
                    .args(&["mouseup", &btn_str])
                    .status();
            }
            nodeinnet_p2p::p2p::DesktopInputEvent::MouseScroll { dy } => {
                let click_btn = if *dy > 0 { "4" } else { "5" };
                let clicks = dy.abs();
                for _ in 0..clicks {
                    let _ = std::process::Command::new("xdotool")
                        .args(&["click", click_btn])
                        .status();
                }
            }
            nodeinnet_p2p::p2p::DesktopInputEvent::KeyPress { keyval } => {
                let _ = std::process::Command::new("xdotool")
                    .args(&["keydown", &format!("0x{:x}", keyval)])
                    .status();
            }
            nodeinnet_p2p::p2p::DesktopInputEvent::KeyRelease { keyval } => {
                let _ = std::process::Command::new("xdotool")
                    .args(&["keyup", &format!("0x{:x}", keyval)])
                    .status();
            }
        }
    }

    #[cfg(target_os = "windows")]
    fn gdk_keyval_to_windows_vk(keyval: u32) -> Option<u16> {
        match keyval {
            // Alphanumeric keys (ASCII mapping for basic letters/numbers)
            0x030..=0x039 => Some((keyval - 0x030 + 0x30) as u16), // '0'..'9'
            0x041..=0x05a => Some(keyval as u16),                  // 'A'..'Z'
            0x061..=0x07a => Some((keyval - 0x061 + 0x41) as u16), // 'a'..'z' -> VK_A..VK_Z

            // Control Keys
            0xff08 => Some(0x08),          // BackSpace (VK_BACK)
            0xff09 => Some(0x09),          // Tab (VK_TAB)
            0xff0d | 0xff8d => Some(0x0d), // Return / KP_Enter (VK_RETURN)
            0xff1b => Some(0x1b),          // Escape (VK_ESCAPE)
            0x020 => Some(0x20),           // Space (VK_SPACE)
            0xff55 => Some(0x21),          // PageUp (VK_PRIOR)
            0xff56 => Some(0x22),          // PageDown (VK_NEXT)
            0xff57 => Some(0x23),          // End (VK_END)
            0xff50 => Some(0x24),          // Home (VK_HOME)
            0xff51 => Some(0x25),          // Left (VK_LEFT)
            0xff52 => Some(0x26),          // Up (VK_UP)
            0xff53 => Some(0x27),          // Right (VK_RIGHT)
            0xff54 => Some(0x28),          // Down (VK_DOWN)
            0xff63 => Some(0x2d),          // Insert (VK_INSERT)
            0xffff => Some(0x2e),          // Delete (VK_DELETE)

            // Modifiers
            0xffe1 | 0xffe2 => Some(0x10), // Shift L/R (VK_SHIFT)
            0xffe3 | 0xffe4 => Some(0x11), // Control L/R (VK_CONTROL)
            0xffe5 => Some(0x14),          // Caps Lock (VK_CAPITAL)
            0xffe9 | 0xffea => Some(0x12), // Alt L/R (VK_MENU)

            _ => None,
        }
    }

    #[cfg(target_os = "macos")]
    fn gdk_keyval_to_macos_code(keyval: u32) -> Option<u16> {
        match keyval {
            // Letters
            0x061 | 0x041 => Some(0),  // A
            0x073 | 0x053 => Some(1),  // S
            0x064 | 0x044 => Some(2),  // D
            0x066 | 0x046 => Some(3),  // F
            0x068 | 0x048 => Some(4),  // H
            0x067 | 0x047 => Some(5),  // G
            0x07a | 0x05a => Some(6),  // Z
            0x078 | 0x058 => Some(7),  // X
            0x063 | 0x043 => Some(8),  // C
            0x076 | 0x056 => Some(9),  // V
            0x062 | 0x042 => Some(11), // B
            0x071 | 0x051 => Some(12), // Q
            0x077 | 0x057 => Some(13), // W
            0x065 | 0x045 => Some(14), // E
            0x072 | 0x052 => Some(15), // R
            0x079 | 0x059 => Some(16), // Y
            0x074 | 0x054 => Some(17), // T
            0x06f | 0x04f => Some(31), // O
            0x069 | 0x049 => Some(34), // I
            0x070 | 0x050 => Some(35), // P
            0x06c | 0x04c => Some(37), // L
            0x06a | 0x04a => Some(38), // J
            0x06b | 0x04b => Some(40), // K
            0x06e | 0x04e => Some(45), // N
            0x06d | 0x04d => Some(46), // M

            // Numbers
            0x031 => Some(18), // 1
            0x032 => Some(19), // 2
            0x033 => Some(20), // 3
            0x034 => Some(21), // 4
            0x035 => Some(23), // 5
            0x036 => Some(22), // 6
            0x037 => Some(26), // 7
            0x038 => Some(28), // 8
            0x039 => Some(25), // 9
            0x030 => Some(29), // 0

            // Navigation / Control
            0xff0d | 0xff8d => Some(36), // Enter
            0xff09 => Some(48),          // Tab
            0x020 => Some(49),           // Space
            0xff08 => Some(51),          // Backspace
            0xff1b => Some(53),          // Escape
            0xff51 => Some(123),         // Left Arrow
            0xff53 => Some(124),         // Right Arrow
            0xff54 => Some(125),         // Down Arrow
            0xff52 => Some(126),         // Up Arrow

            // Modifiers
            0xffe1 | 0xffe2 => Some(56), // Shift
            0xffe3 | 0xffe4 => Some(59), // Control
            0xffe9 | 0xffea => Some(58), // Alt/Option

            _ => None,
        }
    }

    #[cfg(target_os = "windows")]
    {
        use std::mem::size_of;
        unsafe {
            match event {
                nodeinnet_p2p::p2p::DesktopInputEvent::MouseMove { x, y } => {
                    let screen_w = win32_input::GetSystemMetrics(win32_input::SM_CXSCREEN);
                    let screen_h = win32_input::GetSystemMetrics(win32_input::SM_CYSCREEN);

                    let abs_x = if screen_w > 0 {
                        ((*x as f64) / (screen_w as f64) * 65535.0) as i32
                    } else {
                        0
                    };
                    let abs_y = if screen_h > 0 {
                        ((*y as f64) / (screen_h as f64) * 65535.0) as i32
                    } else {
                        0
                    };

                    let mut input = win32_input::INPUT {
                        type_: win32_input::INPUT_MOUSE,
                        u: std::mem::zeroed(),
                    };
                    input.u.mi = win32_input::MOUSEINPUT {
                        dx: abs_x as std::os::raw::c_long,
                        dy: abs_y as std::os::raw::c_long,
                        mouseData: 0,
                        dwFlags: win32_input::MOUSEEVENTF_ABSOLUTE | win32_input::MOUSEEVENTF_MOVE,
                        time: 0,
                        dwExtraInfo: 0,
                    };
                    win32_input::SendInput(1, &input, size_of::<win32_input::INPUT>() as i32);
                }
                nodeinnet_p2p::p2p::DesktopInputEvent::MouseDown { button } => {
                    let flags = match button {
                        1 => win32_input::MOUSEEVENTF_LEFTDOWN,
                        2 => win32_input::MOUSEEVENTF_RIGHTDOWN,
                        3 => win32_input::MOUSEEVENTF_MIDDLEDOWN,
                        _ => return,
                    };
                    let mut input = win32_input::INPUT {
                        type_: win32_input::INPUT_MOUSE,
                        u: std::mem::zeroed(),
                    };
                    input.u.mi = win32_input::MOUSEINPUT {
                        dx: 0,
                        dy: 0,
                        mouseData: 0,
                        dwFlags: flags,
                        time: 0,
                        dwExtraInfo: 0,
                    };
                    win32_input::SendInput(1, &input, size_of::<win32_input::INPUT>() as i32);
                }
                nodeinnet_p2p::p2p::DesktopInputEvent::MouseUp { button } => {
                    let flags = match button {
                        1 => win32_input::MOUSEEVENTF_LEFTUP,
                        2 => win32_input::MOUSEEVENTF_RIGHTUP,
                        3 => win32_input::MOUSEEVENTF_MIDDLEUP,
                        _ => return,
                    };
                    let mut input = win32_input::INPUT {
                        type_: win32_input::INPUT_MOUSE,
                        u: std::mem::zeroed(),
                    };
                    input.u.mi = win32_input::MOUSEINPUT {
                        dx: 0,
                        dy: 0,
                        mouseData: 0,
                        dwFlags: flags,
                        time: 0,
                        dwExtraInfo: 0,
                    };
                    win32_input::SendInput(1, &input, size_of::<win32_input::INPUT>() as i32);
                }
                nodeinnet_p2p::p2p::DesktopInputEvent::MouseScroll { dy } => {
                    let mut input = win32_input::INPUT {
                        type_: win32_input::INPUT_MOUSE,
                        u: std::mem::zeroed(),
                    };
                    input.u.mi = win32_input::MOUSEINPUT {
                        dx: 0,
                        dy: 0,
                        mouseData: (*dy * 120) as u32,
                        dwFlags: win32_input::MOUSEEVENTF_WHEEL,
                        time: 0,
                        dwExtraInfo: 0,
                    };
                    win32_input::SendInput(1, &input, size_of::<win32_input::INPUT>() as i32);
                }
                nodeinnet_p2p::p2p::DesktopInputEvent::KeyPress { keyval } => {
                    if let Some(vk) = gdk_keyval_to_windows_vk(*keyval) {
                        let mut input = win32_input::INPUT {
                            type_: win32_input::INPUT_KEYBOARD,
                            u: std::mem::zeroed(),
                        };
                        input.u.ki = win32_input::KEYBDINPUT {
                            wVk: vk,
                            wScan: 0,
                            dwFlags: 0,
                            time: 0,
                            dwExtraInfo: 0,
                        };
                        win32_input::SendInput(1, &input, size_of::<win32_input::INPUT>() as i32);
                    }
                }
                nodeinnet_p2p::p2p::DesktopInputEvent::KeyRelease { keyval } => {
                    if let Some(vk) = gdk_keyval_to_windows_vk(*keyval) {
                        let mut input = win32_input::INPUT {
                            type_: win32_input::INPUT_KEYBOARD,
                            u: std::mem::zeroed(),
                        };
                        input.u.ki = win32_input::KEYBDINPUT {
                            wVk: vk,
                            wScan: 0,
                            dwFlags: win32_input::KEYEVENTF_KEYUP,
                            time: 0,
                            dwExtraInfo: 0,
                        };
                        win32_input::SendInput(1, &input, size_of::<win32_input::INPUT>() as i32);
                    }
                }
            }
        }
    }

    #[cfg(target_os = "macos")]
    {
        check_and_request_accessibility_permissions();
        unsafe {
            match event {
                nodeinnet_p2p::p2p::DesktopInputEvent::MouseMove { x, y } => {
                    let pos = macos_input::CGPoint {
                        x: *x as f64,
                        y: *y as f64,
                    };
                    let ev = macos_input::CGEventCreateMouseEvent(
                        std::ptr::null_mut(),
                        macos_input::CGEventMouseMoved,
                        pos,
                        macos_input::CGMouseButtonLeft,
                    );
                    if !ev.is_null() {
                        macos_input::CGEventPost(macos_input::CGHIDEventTap, ev);
                        macos_input::CFRelease(ev);
                    }
                }
                nodeinnet_p2p::p2p::DesktopInputEvent::MouseDown { button } => {
                    let current_pos = {
                        let dummy_ev = macos_input::CGEventCreate(std::ptr::null_mut());
                        if !dummy_ev.is_null() {
                            let p = macos_input::CGEventGetLocation(dummy_ev);
                            macos_input::CFRelease(dummy_ev);
                            p
                        } else {
                            macos_input::CGPoint { x: 0.0, y: 0.0 }
                        }
                    };
                    let (mouse_type, cg_btn) = match button {
                        1 => (
                            macos_input::CGEventLeftMouseDown,
                            macos_input::CGMouseButtonLeft,
                        ),
                        2 => (
                            macos_input::CGEventRightMouseDown,
                            macos_input::CGMouseButtonRight,
                        ),
                        3 => (
                            macos_input::CGEventOtherMouseDown,
                            macos_input::CGMouseButtonCenter,
                        ),
                        _ => return,
                    };
                    let ev = macos_input::CGEventCreateMouseEvent(
                        std::ptr::null_mut(),
                        mouse_type,
                        current_pos,
                        cg_btn,
                    );
                    if !ev.is_null() {
                        macos_input::CGEventPost(macos_input::CGHIDEventTap, ev);
                        macos_input::CFRelease(ev);
                    }
                }
                nodeinnet_p2p::p2p::DesktopInputEvent::MouseUp { button } => {
                    let current_pos = {
                        let dummy_ev = macos_input::CGEventCreate(std::ptr::null_mut());
                        if !dummy_ev.is_null() {
                            let p = macos_input::CGEventGetLocation(dummy_ev);
                            macos_input::CFRelease(dummy_ev);
                            p
                        } else {
                            macos_input::CGPoint { x: 0.0, y: 0.0 }
                        }
                    };
                    let (mouse_type, cg_btn) = match button {
                        1 => (
                            macos_input::CGEventLeftMouseUp,
                            macos_input::CGMouseButtonLeft,
                        ),
                        2 => (
                            macos_input::CGEventRightMouseUp,
                            macos_input::CGMouseButtonRight,
                        ),
                        3 => (
                            macos_input::CGEventOtherMouseUp,
                            macos_input::CGMouseButtonCenter,
                        ),
                        _ => return,
                    };
                    let ev = macos_input::CGEventCreateMouseEvent(
                        std::ptr::null_mut(),
                        mouse_type,
                        current_pos,
                        cg_btn,
                    );
                    if !ev.is_null() {
                        macos_input::CGEventPost(macos_input::CGHIDEventTap, ev);
                        macos_input::CFRelease(ev);
                    }
                }
                nodeinnet_p2p::p2p::DesktopInputEvent::MouseScroll { dy } => {
                    let ev = macos_input::CGEventCreateScrollWheelEvent2(
                        std::ptr::null_mut(),
                        macos_input::kCGScrollEventUnitLine,
                        1,
                        *dy,
                        0,
                        0,
                    );
                    if !ev.is_null() {
                        macos_input::CGEventPost(macos_input::CGHIDEventTap, ev);
                        macos_input::CFRelease(ev);
                    }
                }
                nodeinnet_p2p::p2p::DesktopInputEvent::KeyPress { keyval } => {
                    if let Some(code) = gdk_keyval_to_macos_code(*keyval) {
                        let ev = macos_input::CGEventCreateKeyboardEvent(
                            std::ptr::null_mut(),
                            code,
                            true,
                        );
                        if !ev.is_null() {
                            macos_input::CGEventPost(macos_input::CGHIDEventTap, ev);
                            macos_input::CFRelease(ev);
                        }
                    }
                }
                nodeinnet_p2p::p2p::DesktopInputEvent::KeyRelease { keyval } => {
                    if let Some(code) = gdk_keyval_to_macos_code(*keyval) {
                        let ev = macos_input::CGEventCreateKeyboardEvent(
                            std::ptr::null_mut(),
                            code,
                            false,
                        );
                        if !ev.is_null() {
                            macos_input::CGEventPost(macos_input::CGHIDEventTap, ev);
                            macos_input::CFRelease(ev);
                        }
                    }
                }
            }
        }
    }
}

/// Checks if the application has macOS Accessibility permissions, and requests them if missing.
/// On other platforms, this is a no-op that returns true.
pub fn check_and_request_accessibility_permissions() -> bool {
    #[cfg(target_os = "macos")]
    {
        static HAS_PROMPTED: std::sync::atomic::AtomicBool =
            std::sync::atomic::AtomicBool::new(false);
        unsafe {
            if macos_input::AXIsProcessTrusted() {
                return true;
            }
            if HAS_PROMPTED.swap(true, std::sync::atomic::Ordering::Relaxed) {
                return false;
            }
            let keys = [macos_input::kAXTrustedCheckOptionPrompt as *const std::os::raw::c_void];
            let values = [macos_input::kCFBooleanTrue as *const std::os::raw::c_void];
            let dict = macos_input::CFDictionaryCreate(
                std::ptr::null(),
                keys.as_ptr(),
                values.as_ptr(),
                1,
                &macos_input::kCFTypeDictionaryKeyCallBacks as *const std::os::raw::c_void,
                &macos_input::kCFTypeDictionaryValueCallBacks as *const std::os::raw::c_void,
            );
            if !dict.is_null() {
                let trusted = macos_input::AXIsProcessTrustedWithOptions(dict);
                macos_input::CFRelease(dict as *mut std::os::raw::c_void);
                trusted
            } else {
                false
            }
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        true
    }
}
