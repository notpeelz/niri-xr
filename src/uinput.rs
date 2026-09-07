use std::fs::File;
use std::time::{SystemTime, UNIX_EPOCH};

use eyre::{Context, Result};
use input_linux::{EventKind, InputId, Key, UInputHandle};

const EV_KEY: u16 = 0x1;
const EV_SYN: u16 = 0x0;
const SYN_REPORT: u16 = 0x0;

const KEY_CAPSLOCK: u16 = 58;
const KEY_TAB: u16 = 15;

pub struct VirtualKeyboard {
    handle: UInputHandle<File>,
}

impl VirtualKeyboard {
    pub fn new() -> Result<Self> {
        let keyboard_file = File::options()
            .write(true)
            .open("/dev/uinput")
            .wrap_err_with(|| "could not create uinput device")?;
        let handle = UInputHandle::new(keyboard_file);

        let id = InputId {
            bustype: 0x03,
            vendor: 0x4711,
            product: 0x0829,
            version: 5,
        };

        handle.set_evbit(EventKind::Key)?;
        for key in [KEY_CAPSLOCK, KEY_TAB] {
            handle.set_keybit(Key::from_code(key)?)?;
        }
        handle.create(&id, b"NiriXR Keyboard\0", 0, &[])?;

        Ok(Self { handle })
    }

    fn send_key(&self, code: u16, down: bool) -> Result<()> {
        let events = [key_event(code, down), sync_event()];
        self.handle
            .write(&events)
            .wrap_err("failed to write key event")?;
        Ok(())
    }

    pub fn toggle_overview(&self) -> Result<()> {
        self.send_key(KEY_CAPSLOCK, true)?;
        self.send_key(KEY_TAB, true)?;
        self.send_key(KEY_TAB, false)?;
        self.send_key(KEY_CAPSLOCK, false)?;
        Ok(())
    }
}

fn now_timeval() -> libc::timeval {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    libc::timeval {
        tv_sec: now.as_secs() as libc::time_t,
        tv_usec: now.subsec_micros() as libc::suseconds_t,
    }
}

fn key_event(code: u16, down: bool) -> libc::input_event {
    libc::input_event {
        time: now_timeval(),
        type_: EV_KEY,
        code,
        value: down.into(),
    }
}

fn sync_event() -> libc::input_event {
    libc::input_event {
        time: now_timeval(),
        type_: EV_SYN,
        code: SYN_REPORT,
        value: 0,
    }
}
