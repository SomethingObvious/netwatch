use super::*;

pub enum Kind { Step(u16, u16), Choice(&'static [u16]), Toggle }

pub struct Setting { key: &'static str, name: &'static str, hint: &'static str, kind: Kind, default: u16 }

pub const BAD_LOST: usize = 0;
pub const RESET_AFTER: usize = 1;
pub const RECOVERY: usize = 2;
pub const READINGS: usize = 3;
pub const STRIP: usize = 4;
pub const SIZE: usize = 5;
pub const REFRESH: usize = 6;
pub const FLIP: usize = 7;
pub const DOWN_UP: usize = 8;
pub const CPU: usize = 9;
pub const RAM: usize = 10;
pub const GPU: usize = 11;
pub const BATTERY: usize = 12;
pub const LOCK: usize = 13;
pub const STARTUP: usize = 14;
const COUNT: usize = 15;

const SETTINGS: [Setting; COUNT] = [
    Setting { key: "bad_lookups", name: "Bad minute", hint: "Lookups missed in a minute, both hosts", kind: Kind::Step(1, 60), default: 5 },
    Setting { key: "reset_stretch", name: "Reset after", hint: "Bad minutes within 15 before it says RESET", kind: Kind::Step(2, 15), default: 5 },
    Setting { key: "recovery", name: "Recovery after", hint: "Quiet minutes after the last bad one", kind: Kind::Choice(&[5, 10, 15, 20, 30]), default: 10 },
    Setting { key: "readings", name: "Readings", hint: "Loss, ping, router and bad minutes", kind: Kind::Toggle, default: 1 },
    Setting { key: "hour_strip", name: "Hour strip", hint: "The last 60 minutes, one cell each", kind: Kind::Toggle, default: 1 },
    Setting { key: "size", name: "Size", hint: "Percent of the normal board", kind: Kind::Choice(&[100, 125, 150, 200]), default: 100 },
    Setting { key: "number_updates", name: "Number updates", hint: "Seconds between refreshes of the numbers", kind: Kind::Choice(&[5, 10, 30, 60]), default: 10 },
    Setting { key: "flip", name: "Flip animation", hint: "Off changes values in place", kind: Kind::Toggle, default: 1 },
    Setting { key: "down_up", name: "Down and up", hint: "Mbps on the adapter the checks go out on", kind: Kind::Toggle, default: 0 },
    Setting { key: "cpu", name: "CPU", hint: "Percent busy across all cores", kind: Kind::Toggle, default: 0 },
    Setting { key: "ram", name: "RAM", hint: "Percent of memory in use", kind: Kind::Toggle, default: 0 },
    Setting { key: "gpu", name: "GPU", hint: "Busiest engine, as Task Manager shows it", kind: Kind::Toggle, default: 0 },
    Setting { key: "battery", name: "Battery", hint: "Charge left, amber under 20%", kind: Kind::Toggle, default: 0 },
    Setting { key: "lock", name: "Lock position", hint: "Stops dragging. A click still opens this", kind: Kind::Toggle, default: 0 },
    Setting { key: "", name: "Start with Windows", hint: "Runs it when you sign in", kind: Kind::Toggle, default: 0 },
];

const SECTIONS: [(usize, &str); 4] = [(BAD_LOST, "WHAT COUNTS"), (READINGS, "WHAT'S SHOWN"), (DOWN_UP, "THIS PC"), (LOCK, "DESKTOP")];

const fn defaults() -> [u16; COUNT] {
    let mut v = [0; COUNT];
    let mut i = 0;
    while i < COUNT {
        v[i] = SETTINGS[i].default;
        i += 1;
    }
    v
}

static VALUES: Mutex<[u16; COUNT]> = Mutex::new(defaults());

pub fn get(i: usize) -> u16 { VALUES.lock().unwrap()[i] }
pub fn on(i: usize) -> bool { get(i) != 0 }
pub fn set(i: usize, v: u16) { VALUES.lock().unwrap()[i] = v }

fn valid(i: usize, v: u16) -> bool {
    match SETTINGS[i].kind {
        Kind::Step(lo, hi) => (lo..=hi).contains(&v),
        Kind::Choice(c) => c.contains(&v),
        Kind::Toggle => v <= 1,
    }
}

fn stepped(i: usize, v: u16, up: bool) -> u16 {
    match SETTINGS[i].kind {
        Kind::Step(lo, hi) => if up { (v + 1).min(hi) } else { v.saturating_sub(1).max(lo) },
        Kind::Choice(c) => {
            let at = c.iter().position(|&x| x == v).unwrap_or(0);
            c[if up { (at + 1).min(c.len() - 1) } else { at.saturating_sub(1) }]
        }
        Kind::Toggle => 1 - v.min(1),
    }
}

fn parse(text: &str) -> [u16; COUNT] {
    let mut v = defaults();
    for line in text.lines() {
        let Some((key, value)) = line.split_once(' ') else { continue };
        let i = SETTINGS.iter().position(|s| !s.key.is_empty() && s.key == key);
        if let (Some(i), Ok(x)) = (i, value.trim().parse::<u16>()) {
            if valid(i, x) { v[i] = x }
        }
    }
    v
}

fn text(v: &[u16; COUNT]) -> String {
    SETTINGS.iter().zip(v).filter(|(s, _)| !s.key.is_empty()).map(|(s, x)| format!("{} {x}\n", s.key)).collect()
}

pub fn load() {
    let saved = std::fs::read_to_string(data_dir().join("settings.txt")).unwrap_or_default();
    *VALUES.lock().unwrap() = parse(&saved);
}

fn save() {
    let saved = text(&VALUES.lock().unwrap());
    let _ = std::fs::write(data_dir().join("settings.txt"), saved);
}

static PAGE: AtomicIsize = AtomicIsize::new(0);
static HOVER: AtomicIsize = AtomicIsize::new(-1);

const PW: i32 = 300;
const TOP: i32 = 40;
const HEAD: i32 = 24;
const ROW: i32 = 32;
const FOOT: i32 = TOP + HEAD * SECTIONS.len() as i32 + ROW * COUNT as i32 + 8;
const PH: i32 = FOOT + 26 + 14;
const VALUE_X: i32 = 218;
const NAME_FACE: &str = "Bahnschrift SemiBold SemiConden";
const HINT_FACE: &str = "Bahnschrift SemiCondensed";

fn row_top(i: usize) -> i32 {
    TOP + HEAD * SECTIONS.iter().filter(|s| s.0 <= i).count() as i32 + ROW * i as i32
}

#[derive(Clone, Copy, PartialEq)]
enum Hit { Less(usize), More(usize), Flip(usize), ClearHour, Quit, Close }

fn items() -> Vec<(Hit, [i32; 4])> {
    let mut v = vec![(Hit::Close, [264, 8, 24, 24])];
    for (i, s) in SETTINGS.iter().enumerate() {
        let y = row_top(i);
        match s.kind {
            Kind::Toggle => v.push((Hit::Flip(i), [VALUE_X - 20, y + 2, 88, 26])),
            _ => {
                v.push((Hit::Less(i), [VALUE_X - 20, y + 6, 18, 18]));
                v.push((Hit::More(i), [VALUE_X + 50, y + 6, 18, 18]));
            }
        }
    }
    v.push((Hit::ClearHour, [16, FOOT, 130, 26]));
    v.push((Hit::Quit, [154, FOOT, 130, 26]));
    v
}

unsafe fn hit_at(hwnd: H, x: i32, y: i32) -> isize {
    let s = GetDpiForWindow(hwnd) as f32 / 96.0;
    let inside = |[l, t, w, h]: [i32; 4]| {
        x as f32 >= l as f32 * s && (x as f32) < (l + w) as f32 * s && y as f32 >= t as f32 * s && (y as f32) < (t + h) as f32 * s
    };
    items().iter().position(|(_, r)| inside(*r)).map_or(-1, |i| i as isize)
}

fn lparam_point(lp: isize) -> (i32, i32) {
    ((lp & 0xFFFF) as i16 as i32, ((lp >> 16) & 0xFFFF) as i16 as i32)
}

pub unsafe fn close() {
    let page = PAGE.load(Relaxed) as H;
    if !page.is_null() { DestroyWindow(page); }
}

pub unsafe fn open(widget: H) {
    let page = PAGE.load(Relaxed) as H;
    if page.is_null() { toggle(widget) } else { SetForegroundWindow(page); }
}

pub unsafe fn toggle(widget: H) {
    if PAGE.load(Relaxed) != 0 { return close() }
    VALUES.lock().unwrap()[STARTUP] = starts_with_windows() as u16;
    let page = CreateWindowExW(WS_EX_TOOLWINDOW, wide("NetwatchSettings").as_ptr(), wide("Netwatch Settings").as_ptr(),
        WS_POPUP, 0, 0, 0, 0, null_mut(), null_mut(), GetModuleHandleW(null()), null_mut());
    if page.is_null() { return }
    PAGE.store(page as isize, Relaxed);
    let corners: u32 = 3;
    DwmSetWindowAttribute(page, 33, (&corners as *const u32).cast(), 4);
    place_page(page, widget);
    ShowWindow(page, 5);
    SetForegroundWindow(page);
}

pub fn size(dpi: u32) -> (i32, i32) {
    let s = dpi as f32 / 96.0;
    ((PW as f32 * s).round() as i32, (PH as f32 * s).round() as i32)
}

unsafe fn place_page(page: H, widget: H) {
    let s = GetDpiForWindow(widget) as f32 / 96.0;
    let (w, h) = size(GetDpiForWindow(widget));
    let mut board: Rect = zeroed();
    GetWindowRect(widget, &mut board);
    let mut info: MonitorInfo = zeroed();
    info.size = size_of::<MonitorInfo>() as u32;
    GetMonitorInfoW(MonitorFromWindow(widget, 2), &mut info);
    let work = info.work;
    let gap = (8.0 * s) as i32;
    let x = (board.r - w).min(work.r - w).max(work.l);
    let y = if board.t - gap - h >= work.t { board.t - gap - h }
        else if board.b + gap + h <= work.b { board.b + gap }
        else { work.t.max(work.b - h) };
    SetWindowPos(page, null_mut(), x, y, w, h, SWP_NOZORDER | SWP_NOACTIVATE);
}

unsafe fn act(page: H, hit: Hit) {
    let widget = WINDOW.load(Relaxed) as H;
    match hit {
        Hit::Close => { DestroyWindow(page); return }
        Hit::Quit => {
            DestroyWindow(page);
            PostMessageW(widget, WM_CLOSE, 0, 0);
            return;
        }
        Hit::ClearHour => {
            *HOUR.lock().unwrap() = [EMPTY; 60];
            save_hour();
        }
        Hit::Flip(STARTUP) => {
            set_starts_with_windows(!on(STARTUP));
            VALUES.lock().unwrap()[STARTUP] = starts_with_windows() as u16;
        }
        Hit::Less(i) | Hit::More(i) | Hit::Flip(i) => {
            {
                let mut v = VALUES.lock().unwrap();
                v[i] = stepped(i, v[i], !matches!(hit, Hit::Less(_)));
            }
            save();
            if [SIZE, READINGS, STRIP, DOWN_UP, CPU, RAM, GPU, BATTERY].contains(&i) {
                resize(widget);
                place_page(page, widget);
            }
        }
    }
    PostMessageW(widget, WM_APP, 0, 0);
    InvalidateRect(page, null(), 0);
}

pub unsafe extern "system" fn proc(hwnd: H, msg: u32, wp: usize, lp: isize) -> isize {
    match msg {
        WM_ERASEBKGND => 1,
        WM_PAINT => {
            paint(hwnd, GetDpiForWindow(hwnd) as f32 / 96.0, |p, w, h| draw(p, w, h));
            0
        }
        WM_MOUSEMOVE => {
            let (x, y) = lparam_point(lp);
            let at = hit_at(hwnd, x, y);
            if HOVER.swap(at, Relaxed) != at { InvalidateRect(hwnd, null(), 0); }
            let mut track = TrackMouse { size: size_of::<TrackMouse>() as u32, flags: TME_LEAVE, hwnd, hover: 0 };
            TrackMouseEvent(&mut track);
            0
        }
        WM_MOUSELEAVE => {
            HOVER.store(-1, Relaxed);
            InvalidateRect(hwnd, null(), 0);
            0
        }
        WM_LBUTTONDOWN => {
            let (x, y) = lparam_point(lp);
            let at = hit_at(hwnd, x, y);
            if at >= 0 { act(hwnd, items()[at as usize].0) }
            0
        }
        WM_SETCURSOR => {
            let mut p: Point = zeroed();
            GetCursorPos(&mut p);
            ScreenToClient(hwnd, &mut p);
            if hit_at(hwnd, p.x, p.y) >= 0 {
                SetCursor(LoadCursorW(null_mut(), IDC_HAND));
                1
            } else {
                DefWindowProcW(hwnd, msg, wp, lp)
            }
        }
        WM_KEYDOWN if wp == VK_ESCAPE => { DestroyWindow(hwnd); 0 }
        WM_ACTIVATE if wp & 0xFFFF == 0 => { PostMessageW(hwnd, WM_CLOSE, 0, 0); 0 }
        WM_DESTROY => {
            PAGE.store(0, Relaxed);
            HOVER.store(-1, Relaxed);
            0
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

pub unsafe fn draw(p: &Painter, w: i32, h: i32) {
    frame(p, w, h);
    let _ = (w, h);
    let list = items();
    let hover = HOVER.load(Relaxed);
    let lit = |hit: Hit| hover >= 0 && list.get(hover as usize).is_some_and(|x| x.0 == hit);
    let rect = |hit: Hit| list.iter().find(|x| x.0 == hit).map_or([0; 4], |x| x.1);
    let button = |hit: Hit, text: &str, size: f32, tracking: i32| {
        let [x, y, bw, bh] = rect(hit);
        p.fill(p.px(x), p.px(y), p.px(x + bw), p.px(y + bh), if lit(hit) { HINGE } else { FRAME });
        p.fill(p.px(x), p.px(y), p.px(x + bw), p.px(y) + p.px(1).max(1), if lit(hit) { STEEL_EDGE } else { HINGE });
        p.text(text, (x, y, bw, bh), size, LETTER, DT_CENTER, NAME_FACE, tracking);
    };

    p.text("NETWATCH SETTINGS", (16, 12, 220, 16), 10.0, LETTER, LEFT, NAME_FACE, 1);
    let [x, y, cw, ch] = rect(Hit::Close);
    p.text("\u{d7}", (x, y, cw, ch), 18.0, if lit(Hit::Close) { LETTER } else { LABEL }, DT_CENTER, HINT_FACE, 0);

    for (row, head) in SECTIONS {
        let y = row_top(row) - HEAD;
        p.label(head, 16, y + 9, 200, LEFT);
        p.fill(p.px(16), p.px(y + 21), p.px(PW - 14), p.px(y + 21) + p.px(1).max(1), HINGE);
    }

    for (i, s) in SETTINGS.iter().enumerate() {
        let y = row_top(i);
        p.text(s.name, (16, y + 3, 176, 15), 12.5, LETTER, LEFT, NAME_FACE, 0);
        p.text(s.hint, (16, y + 18, 182, 12), 9.5, LABEL, LEFT, HINT_FACE, 0);
        let v = get(i);
        let toggle = matches!(s.kind, Kind::Toggle);
        let value: Vec<char> = if toggle { if v != 0 { "ON" } else { "OFF" }.chars().collect() } else { v.to_string().chars().collect() };
        if toggle && lit(Hit::Flip(i)) {
            p.fill(p.px(VALUE_X - 2), p.px(y + 2), p.px(VALUE_X + 48), p.px(y + 28), HINGE);
        }
        let colour = if toggle && v == 0 { LABEL } else { LETTER };
        let pad = 3usize.saturating_sub(value.len());
        for k in 0..3 {
            let c = if k >= pad { value[k - pad] } else { ' ' };
            p.flap(VALUE_X + k as i32 * SMALL.2, y + 4, SMALL, c, c, false, colour);
        }
        if !toggle {
            for hit in [Hit::Less(i), Hit::More(i)] {
                button(hit, "", 0.0, 0);
                let [x, y, bw, bh] = rect(hit);
                let (cx, cy, arm, bar) = (p.px(x) + p.px(bw) / 2, p.px(y) + p.px(bh) / 2, p.px(4), p.px(1).max(2) / 2);
                p.fill(cx - arm, cy - bar, cx + arm, cy + bar + 1, LETTER);
                if hit == Hit::More(i) { p.fill(cx - bar, cy - arm, cx + bar + 1, cy + arm, LETTER) }
            }
        }
    }

    button(Hit::ClearHour, "CLEAR THE HOUR", 8.5, 1);
    button(Hit::Quit, "QUIT NETWATCH", 8.5, 1);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn saved_settings() {
        assert_eq!(parse("bad_minute 3\n")[BAD_LOST], 5);
        let v = parse("bad_lookups 7\nsize 125\nflip 0\nreset_stretch 99\nsize2 3\nrecovery x\nlock\n");
        assert_eq!((v[BAD_LOST], v[SIZE], v[FLIP]), (7, 125, 0));
        assert_eq!((v[RESET_AFTER], v[RECOVERY], v[LOCK]), (5, 10, 0));
        assert_eq!(parse(&text(&v)), v);
        assert!(!text(&v).contains("Start"));

        assert_eq!(stepped(SIZE, 150, true), 200);
        assert_eq!(stepped(SIZE, 200, true), 200);
        assert_eq!(stepped(RECOVERY, 5, false), 5);
        assert_eq!(stepped(BAD_LOST, 1, false), 1);
        assert_eq!(stepped(FLIP, 1, true), 0);
        assert!(row_top(COUNT - 1) + ROW <= FOOT);
    }
}
