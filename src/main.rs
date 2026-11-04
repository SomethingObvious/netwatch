#![windows_subsystem = "windows"]
use std::ffi::c_void;
use std::io::Write;
use std::mem::{size_of, zeroed};
use std::ptr::{null, null_mut};
use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering::Relaxed};
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const BAD_MINUTE_LOST: u8 = 2;
const RESET_BAD_MINUTES: usize = 3;
const HOSTS: [[u8; 4]; 2] = [[1, 1, 1, 1], [8, 8, 8, 8]];
const ROUTER: usize = 2;

type H = *mut c_void;

#[derive(Clone, Copy, Default)]
struct Min {
    stamp: u64,
    sent: [u8; 3],
    lost: [u8; 3],
    rtt: [u32; 3],
}

impl Min {
    fn lost(&self) -> u8 {
        if self.sent[0] == 0 || self.sent[1] == 0 { return 0 }
        let line = self.lost[0].min(self.lost[1]);
        let router = self.lost[ROUTER];
        if router > 0 && router < self.sent[ROUTER] { line.saturating_sub(router) } else { line }
    }
    fn sent(&self) -> u8 {
        self.sent[0].max(self.sent[1])
    }
    fn replies(&self, i: usize) -> u32 {
        (self.sent[i] - self.lost[i]) as u32
    }
}

const EMPTY: Min = Min { stamp: 0, sent: [0; 3], lost: [0; 3], rtt: [0; 3] };
static HOUR: Mutex<[Min; 60]> = Mutex::new([EMPTY; 60]);
static WINDOW: AtomicIsize = AtomicIsize::new(0);
static TASKBAR_CREATED: AtomicIsize = AtomicIsize::new(0);
static CLOSED: AtomicBool = AtomicBool::new(false);

#[derive(Debug, PartialEq)]
struct Verdict {
    bad: usize,
    lossy: usize,
    measured: usize,
    recent_bad: bool,
}

impl Verdict {
    fn reset(&self) -> bool { self.bad >= RESET_BAD_MINUTES && self.recent_bad }
}

fn in_window(m: &Min, now: u64, span: u64) -> bool {
    m.stamp + span > now && m.stamp <= now
}

fn verdict(hour: &[Min; 60], now: u64) -> Verdict {
    let mut v = Verdict { bad: 0, lossy: 0, measured: 0, recent_bad: false };
    let measured = || hour.iter().filter(|m| m.sent() > 0 && in_window(m, now, 60));
    let mut last_bad = None;
    for m in measured() {
        v.measured += 1;
        let lost = m.lost();
        if lost > 0 { v.lossy += 1 }
        if lost >= BAD_MINUTE_LOST {
            v.bad += 1;
            last_bad = last_bad.max(Some(m.stamp));
        }
    }
    if let Some(at) = last_bad {
        v.recent_bad = measured().filter(|m| m.stamp > at).count() < 10;
    }
    v
}

#[derive(Debug, PartialEq)]
struct Live {
    loss_pct: Option<f32>,
    ping_ms: Option<u32>,
    router_ms: Option<u32>,
    router_dropping: bool,
}

fn live(hour: &[Min; 60], now: u64) -> Live {
    let (mut sent, mut lost, mut rtt, mut replies, mut r_rtt, mut r_replies, mut r_lost) = (0u32, 0u32, 0u32, 0u32, 0u32, 0u32, 0u32);
    for m in hour.iter().filter(|m| in_window(m, now, 5)) {
        sent += m.sent() as u32;
        lost += m.lost() as u32;
        if in_window(m, now, 2) {
            rtt += m.rtt[0] + m.rtt[1];
            replies += m.replies(0) + m.replies(1);
            r_rtt += m.rtt[ROUTER];
            r_replies += m.replies(ROUTER);
            r_lost += m.lost[ROUTER] as u32;
        }
    }
    Live {
        loss_pct: (sent > 0).then(|| 100.0 * lost as f32 / sent as f32),
        ping_ms: (replies > 0).then(|| rtt / replies),
        router_ms: (r_replies > 0).then(|| r_rtt / r_replies),
        router_dropping: r_lost >= BAD_MINUTE_LOST as u32,
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum State { Measuring, Clean, Fine, Watch, Recovering, Reset }

fn state(v: &Verdict) -> State {
    if v.reset() { State::Reset }
    else if v.bad >= RESET_BAD_MINUTES { State::Recovering }
    else if v.bad > 0 { State::Watch }
    else if v.lossy > 0 { State::Fine }
    else if v.measured > 0 { State::Clean }
    else { State::Measuring }
}

fn now_min() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs() / 60)
}

fn record(host: usize, lost: bool, rtt: u32) {
    let m = now_min();
    let mut hour = HOUR.lock().unwrap();
    if hour.iter().any(|x| x.stamp > m) {
        for x in hour.iter_mut().filter(|x| x.stamp >= m) { *x = EMPTY }
    }
    let slot = &mut hour[(m % 60) as usize];
    if slot.stamp != m {
        *slot = Min { stamp: m, ..EMPTY };
    }
    if slot.sent[host] == u8::MAX { return }
    slot.sent[host] += 1;
    if lost { slot.lost[host] += 1 } else { slot.rtt[host] += rtt }
}

fn data_dir() -> std::path::PathBuf {
    let dir = std::env::var_os("LOCALAPPDATA").map_or_else(std::env::temp_dir, std::path::PathBuf::from).join("Netwatch");
    let _ = std::fs::create_dir_all(&dir);
    dir
}

fn hour_text(hour: &[Min; 60]) -> String {
    hour.iter().filter(|m| m.stamp > 0).map(|m| format!("{} {} {} {} {} {} {} {} {} {}\n",
        m.stamp, m.sent[0], m.sent[1], m.sent[2], m.lost[0], m.lost[1], m.lost[2], m.rtt[0], m.rtt[1], m.rtt[2])).collect()
}

fn parse_hour(text: &str) -> [Min; 60] {
    let mut hour = [EMPTY; 60];
    for line in text.lines() {
        let f: Vec<&str> = line.split(' ').collect();
        if f.len() != 10 { continue }
        let counts: Result<Vec<u8>, _> = f[1..7].iter().map(|x| x.parse::<u8>()).collect();
        let rtt: Result<Vec<u32>, _> = f[7..].iter().map(|x| x.parse::<u32>()).collect();
        let (Ok(stamp), Ok(c), Ok(r)) = (f[0].parse::<u64>(), counts, rtt) else { continue };
        let m = Min { stamp, sent: [c[0], c[1], c[2]], lost: [c[3], c[4], c[5]], rtt: [r[0], r[1], r[2]] };
        if (0..3).any(|i| m.lost[i] > m.sent[i]) { continue }
        hour[(stamp % 60) as usize] = m;
    }
    hour
}

static SAVING: Mutex<()> = Mutex::new(());

fn save_hour() {
    let _held = SAVING.lock().unwrap();
    let text = hour_text(&HOUR.lock().unwrap());
    let (dir, tmp) = (data_dir(), data_dir().join("hour.tmp"));
    let wrote = std::fs::File::create(&tmp).and_then(|mut f| {
        f.write_all(text.as_bytes())?;
        f.sync_all()
    });
    if wrote.is_ok() {
        let _ = std::fs::rename(&tmp, dir.join("hour.txt"));
    }
}

fn router_address() -> Option<u32> {
    let mut row = [0u32; 14];
    let ok = unsafe { GetBestRoute(u32::from_ne_bytes(HOSTS[0]), 0, row.as_mut_ptr()) } == 0;
    (ok && row[3] != 0).then_some(row[3])
}

const TIMED_OUT: u32 = 11010;
const HOST_UNREACHABLE: u32 = 11003;
const RESET_OFFLINE_ROUNDS: u32 = 30;
const SYNC_GRACE: Duration = Duration::from_secs(180);

fn pinger() {
    unsafe {
        let mut icmp = IcmpCreateFile();
        while icmp as isize == -1 {
            std::thread::sleep(Duration::from_secs(1));
            icmp = IcmpCreateFile();
        }
        let data = [0u8; 8];
        let mut reply = [0u64; 16];
        let mut offline_rounds = 0;
        let mut syncing_until: Option<Instant> = None;
        let mut saved = now_min();
        loop {
            if now_min() != saved {
                saved = now_min();
                save_hour();
            }
            let start = Instant::now();
            let mut offline = true;
            let targets = [Some(u32::from_ne_bytes(HOSTS[0])), Some(u32::from_ne_bytes(HOSTS[1])), router_address()];
            for (i, target) in targets.iter().enumerate() {
                let Some(addr) = *target else { continue };
                let timeout = if i == ROUTER { 500 } else { 900 };
                let n = IcmpSendEcho(icmp, addr, data.as_ptr().cast(), 8, null(),
                    reply.as_mut_ptr().cast(), size_of::<[u64; 16]>() as u32, timeout);
                let lost = if n > 0 {
                    (reply[0] >> 32) as u32 != 0
                } else {
                    match GetLastError() { TIMED_OUT | HOST_UNREACHABLE => true, _ => continue }
                };
                offline = false;
                if i != ROUTER {
                    if !lost { syncing_until = None }
                    if lost && syncing_until.is_some_and(|t| Instant::now() < t) { continue }
                }
                record(i, lost, if lost { 0 } else { reply[1] as u32 });
            }
            offline_rounds = if offline { offline_rounds + 1 } else { 0 };
            if offline_rounds == RESET_OFFLINE_ROUNDS {
                *HOUR.lock().unwrap() = [EMPTY; 60];
                syncing_until = Some(Instant::now() + SYNC_GRACE);
            }
            PostMessageW(WINDOW.load(Relaxed) as H, WM_APP, 0, 0);
            std::thread::sleep(Duration::from_secs(1).saturating_sub(start.elapsed()));
        }
    }
}

const HEADLINE: (usize, usize) = (0, 9);
const LOSS: (usize, usize) = (9, 4);
const PING: (usize, usize) = (13, 3);
const ROUTER_CELLS: (usize, usize) = (16, 3);
const BAD: (usize, usize) = (19, 2);
const CELLS: usize = 21;
const FIELDS: [(usize, usize); 5] = [HEADLINE, LOSS, PING, ROUTER_CELLS, BAD];
const DRUM: &str = " ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789.%-/";
const NUMBER_REFRESH: Duration = Duration::from_secs(10);

fn word(s: State) -> &'static str {
    match s {
        State::Reset => "RESET", State::Watch => "WATCH", State::Recovering => "RECOVERY",
        State::Clean => "CLEAN", State::Fine => "FINE", State::Measuring => "WAIT",
    }
}

fn shown_loss(x: f32) -> f32 {
    let tenths = (x * 10.0).round() / 10.0;
    if tenths >= 10.0 { x.round() } else { tenths }
}

fn targets(s: State, l: &Live, bad: usize) -> [char; CELLS] {
    let loss = match l.loss_pct.map(shown_loss) {
        None => "--".into(),
        Some(x) if x >= 10.0 => format!("{x}%"),
        Some(x) => format!("{x:.1}%"),
    };
    let ms = |v: Option<u32>| v.map_or("--".into(), |x| x.min(999).to_string());
    let router = if l.router_dropping { "DRP".into() } else { ms(l.router_ms) };
    let mut t = [' '; CELLS];
    let mut put = |(start, len): (usize, usize), s: &str, right: bool| {
        let chars: Vec<char> = s.chars().take(len).collect();
        let pad = if right { len - chars.len() } else { 0 };
        for (i, c) in chars.into_iter().enumerate() { t[start + pad + i] = c }
    };
    put(HEADLINE, word(s), false);
    put(LOSS, &loss, true);
    put(PING, &ms(l.ping_ms), true);
    put(ROUTER_CELLS, &router, true);
    put(BAD, &bad.min(99).to_string(), true);
    t
}

fn drum_step(c: char, target: char) -> char {
    let drum: Vec<char> = DRUM.chars().collect();
    let at = drum.iter().position(|&d| d == c).unwrap_or(0);
    let to = drum.iter().position(|&d| d == target).unwrap_or(0);
    let distance = (to + drum.len() - at) % drum.len();
    if distance > 8 { drum[(to + drum.len() - 8) % drum.len()] } else { drum[(at + 1) % drum.len()] }
}

struct Board {
    shown: [char; CELLS],
    old: [char; CELLS],
    half: [bool; CELLS],
    wait: [u8; CELLS],
    target: [char; CELLS],
    colour: [u32; 5],
    numbers_at: Option<Instant>,
}

const BLANK_BOARD: Board = Board {
    shown: [' '; CELLS], old: [' '; CELLS], half: [false; CELLS], wait: [0; CELLS], target: [' '; CELLS],
    colour: [LETTER; 5], numbers_at: None,
};
static BOARD: Mutex<Board> = Mutex::new(BLANK_BOARD);

impl Board {
    fn set(&mut self, t: [char; CELLS], colours: [u32; 5], at: Instant) -> bool {
        let numbers_due = self.numbers_at.map_or(true, |n| at - n >= NUMBER_REFRESH);
        if numbers_due { self.numbers_at = Some(at) }
        for (k, (start, len)) in FIELDS.into_iter().enumerate() {
            if start != HEADLINE.0 && !numbers_due { continue }
            self.colour[k] = colours[k];
            for i in start..start + len {
                if self.target[i] != t[i] {
                    self.target[i] = t[i];
                    self.wait[i] = 2 * (i - start) as u8;
                }
            }
        }
        self.busy()
    }

    fn busy(&self) -> bool {
        (0..CELLS).any(|i| self.shown[i] != self.target[i] || self.half[i])
    }

    fn tick(&mut self) -> bool {
        for i in 0..CELLS {
            if self.half[i] { self.half[i] = false; continue }
            if self.shown[i] == self.target[i] { continue }
            if self.wait[i] > 0 { self.wait[i] -= 1; continue }
            self.old[i] = self.shown[i];
            self.shown[i] = drum_step(self.shown[i], self.target[i]);
            self.half[i] = true;
        }
        self.busy()
    }
}

const fn rgb(r: u8, g: u8, b: u8) -> u32 { r as u32 | (g as u32) << 8 | (b as u32) << 16 }

const STEEL: (u8, u8, u8) = (91, 94, 99);
const STEEL_LOW: (u8, u8, u8) = (42, 45, 49);
const STEEL_EDGE: u32 = rgb(122, 125, 130);
const FRAME: u32 = rgb(42, 45, 49);
const GROUND: u32 = rgb(28, 29, 31);
const FLAP_TOP: u32 = rgb(17, 17, 19);
const FLAP_BOTTOM: u32 = rgb(11, 11, 13);
const HINGE: u32 = rgb(52, 54, 58);
const LETTER: u32 = rgb(242, 242, 242);
const LABEL: u32 = rgb(167, 170, 175);
const AMBER: u32 = rgb(255, 176, 0);
const RED: u32 = rgb(232, 38, 58);
const LAMP_OFF: u32 = rgb(46, 47, 50);

const BW: i32 = 284;
const BH: i32 = 134;
const BIG: (i32, i32, i32) = (22, 38, 24);
const SMALL: (i32, i32, i32) = (14, 22, 16);

struct Painter { dc: H, s: f32 }

impl Painter {
    fn px(&self, v: i32) -> i32 { (v as f32 * self.s).round() as i32 }

    unsafe fn fill(&self, l: i32, t: i32, r: i32, b: i32, colour: u32) {
        let brush = CreateSolidBrush(colour);
        FillRect(self.dc, &Rect { l, t, r, b }, brush);
        DeleteObject(brush);
    }

    unsafe fn dot(&self, cx: i32, cy: i32, r: f32, colour: u32) {
        let brush = CreateSolidBrush(colour);
        let old_brush = SelectObject(self.dc, brush);
        let old_pen = SelectObject(self.dc, GetStockObject(8));
        let (x, y, rr) = (cx as f32 * self.s, cy as f32 * self.s, r * self.s);
        Ellipse(self.dc, (x - rr).round() as i32, (y - rr).round() as i32, (x + rr).round() as i32 + 1, (y + rr).round() as i32 + 1);
        SelectObject(self.dc, old_pen);
        SelectObject(self.dc, old_brush);
        DeleteObject(brush);
    }

    unsafe fn text_px(&self, s: &str, r: Rect, face: &str, size: i32, colour: u32, align: u32, tracking: i32) {
        let font = CreateFontW(-size, 0, 0, 0, 600, 0, 0, 0, 1, 0, 0, 5, 0, wide(face).as_ptr());
        let old = SelectObject(self.dc, font);
        SetTextColor(self.dc, colour);
        SetTextCharacterExtra(self.dc, tracking);
        let mut r = r;
        let text = wide(s);
        DrawTextW(self.dc, text.as_ptr(), -1, &mut r, DT_SINGLELINE | DT_VCENTER | DT_NOPREFIX | DT_NOCLIP | align);
        SetTextCharacterExtra(self.dc, 0);
        SelectObject(self.dc, old);
        DeleteObject(font);
    }

    unsafe fn label(&self, s: &str, x: i32, y: i32, w: i32, align: u32) {
        let r = Rect { l: self.px(x), t: self.px(y), r: self.px(x + w), b: self.px(y + 10) };
        self.text_px(s, r, "Bahnschrift SemiBold SemiConden", self.px(8), LABEL, align, self.px(1));
    }

    unsafe fn flap(&self, x: i32, y: i32, (w, h, _): (i32, i32, i32), ch: char, old: char, half: bool, colour: u32) {
        let (l, t, r, b) = (self.px(x), self.px(y), self.px(x + w), self.px(y + h));
        let mid = t + (b - t) / 2;
        self.fill(l, t, r, mid, FLAP_TOP);
        self.fill(l, mid, r, b, FLAP_BOTTOM);
        let size = ((b - t) as f32 * 0.86) as i32;
        let draw = |c: char, top: i32, bottom: i32| {
            if c == ' ' { return }
            SaveDC(self.dc);
            IntersectClipRect(self.dc, l, top, r, bottom);
            self.text_px(&c.to_string(), Rect { l, t: t + 1, r, b: b + 1 }, "Bahnschrift SemiBold Condensed", size, colour, DT_CENTER, 0);
            RestoreDC(self.dc, -1);
        };
        if half {
            draw(ch, t, mid);
            draw(old, mid, b);
        } else {
            draw(ch, t, b);
        }
        let line = self.px(1).max(1);
        self.fill(l, mid, r, mid + line, rgb(0, 0, 0));
        let pin = self.px(2).max(2);
        self.fill(l, mid - pin, l + line, mid + pin, HINGE);
        self.fill(r - line, mid - pin, r, mid + pin, HINGE);
    }
}

fn field_colour(field: (usize, usize), s: State, l: &Live, bad: usize) -> u32 {
    if field == HEADLINE {
        return match s { State::Reset => RED, State::Watch | State::Recovering => AMBER, _ => LETTER };
    }
    if field == LOSS {
        return match l.loss_pct.map(shown_loss) { Some(x) if x >= 10.0 => RED, Some(x) if x >= 3.0 => AMBER, _ => LETTER };
    }
    if field == ROUTER_CELLS && l.router_dropping { return RED }
    if field == BAD {
        return if s == State::Reset { RED } else if bad > 0 { AMBER } else { LETTER };
    }
    LETTER
}

fn colours(s: State, l: &Live, bad: usize) -> [u32; 5] {
    FIELDS.map(|f| field_colour(f, s, l, bad))
}

unsafe fn draw_board(p: &Painter, hour: &[Min; 60], now: u64, board: &Board, w: i32, h: i32) {
    let s = state(&verdict(hour, now));

    let band = 6;
    for y in (0..h).step_by(band) {
        let mix = |a: u8, b: u8| (a as i32 + (b as i32 - a as i32) * y / h.max(1)) as u8;
        p.fill(0, y, w, (y + band as i32).min(h), rgb(mix(STEEL.0, STEEL_LOW.0), mix(STEEL.1, STEEL_LOW.1), mix(STEEL.2, STEEL_LOW.2)));
    }
    let line = p.px(1).max(1);
    p.fill(0, 0, w, line, STEEL_EDGE);
    p.fill(0, h - line, w, h, rgb(18, 19, 21));
    p.fill(p.px(4), p.px(4), w - p.px(4), h - p.px(4), FRAME);
    p.fill(p.px(5), p.px(5), w - p.px(5), h - p.px(5), GROUND);

    let lamp = match s { State::Reset => RED, State::Watch | State::Recovering => AMBER, _ => LAMP_OFF };
    p.dot(258, 30, 5.5, FRAME);
    p.dot(258, 30, 3.5, lamp);

    let cells = |field: (usize, usize), x: i32, y: i32, size: (i32, i32, i32)| {
        let colour = board.colour[FIELDS.iter().position(|&f| f == field).unwrap_or(0)];
        for i in 0..field.1 {
            let c = field.0 + i;
            p.flap(x + i as i32 * size.2, y, size, board.shown[c], board.old[c], board.half[c], colour);
        }
    };
    cells(HEADLINE, 13, 11, BIG);

    let row = 60;
    p.label("LOSS 5 MIN", 13, row, 70, LEFT);
    p.label("PING MS", 90, row, 60, LEFT);
    p.label("ROUTER MS", 150, row, 60, LEFT);
    p.label("BAD / HR", 214, row, 60, LEFT);
    cells(LOSS, 13, row + 11, SMALL);
    cells(PING, 90, row + 11, SMALL);
    cells(ROUTER_CELLS, 150, row + 11, SMALL);
    cells(BAD, 214, row + 11, SMALL);

    let strip = 103;
    p.label("LAST HOUR", 13, strip, 80, LEFT);
    p.label("NOW", 210, strip, 60, RIGHT);
    let (x0, y0, width, height) = (p.px(13), p.px(strip + 12), p.px(BW - 26), p.px(10));
    let gap = p.px(1).max(1);
    for i in 0..60u64 {
        let m = now + i - 59;
        let minute = hour[(m % 60) as usize];
        let colour = if minute.stamp != m || minute.sent() == 0 { rgb(17, 17, 19) }
            else if minute.lost() == 0 { rgb(48, 50, 54) }
            else if minute.lost() < BAD_MINUTE_LOST { LABEL }
            else if s == State::Reset { RED } else { AMBER };
        let x = x0 + i as i32 * width / 60;
        let x1 = x0 + (i as i32 + 1) * width / 60 - gap;
        p.fill(x, y0, x1, y0 + height, colour);
    }
}

unsafe fn paint(hwnd: H) {
    let mut ps: PaintStruct = zeroed();
    let screen = BeginPaint(hwnd, &mut ps);
    let mut client: Rect = zeroed();
    GetClientRect(hwnd, &mut client);
    let dc = CreateCompatibleDC(screen);
    let bmp = CreateCompatibleBitmap(screen, client.r, client.b);
    let old_bmp = SelectObject(dc, bmp);
    SetBkMode(dc, 1);
    let hour = *HOUR.lock().unwrap();
    let p = Painter { dc, s: GetDpiForWindow(hwnd) as f32 / 96.0 };
    draw_board(&p, &hour, now_min(), &BOARD.lock().unwrap(), client.r, client.b);
    BitBlt(screen, 0, 0, client.r, client.b, dc, 0, 0, 0x00CC_0020);
    SelectObject(dc, old_bmp);
    DeleteObject(bmp);
    DeleteDC(dc);
    EndPaint(hwnd, &ps);
}

unsafe fn refresh(hwnd: H) {
    let hour = *HOUR.lock().unwrap();
    let now = now_min();
    let v = verdict(&hour, now);
    let (s, l) = (state(&v), live(&hour, now));
    if BOARD.lock().unwrap().set(targets(s, &l, v.bad), colours(s, &l, v.bad), Instant::now()) {
        SetTimer(hwnd, 1, 30, null());
    }
    InvalidateRect(hwnd, null(), 0);
}

fn sample_hour(state: &str, now: u64) -> [Min; 60] {
    let mut hour = [EMPTY; 60];
    let heavy = [[9, 8], [4, 5], [2, 3], [12, 10], [6, 7], [0, 0], [3, 2], [8, 9], [1, 1], [5, 6]];
    for i in 0..60u64 {
        let m = now - i;
        let lost: [u8; 2] = match state {
            "reset" if i < 24 => heavy[(i % 10) as usize],
            "watch" if i == 22 || i == 23 => [3, 2],
            _ if i == 12 || i == 41 => [1, 1],
            _ => [0, 0],
        };
        let ping = if state == "reset" { 70 } else { 31 };
        hour[(m % 60) as usize] = Min {
            stamp: m, sent: [60; 3], lost: [lost[0], lost[1], 0],
            rtt: [ping * (60 - lost[0] as u32), ping * (60 - lost[1] as u32), 3 * 60],
        };
    }
    hour
}

unsafe fn render(state_name: &str, out: &str, flipping: bool) {
    let dpi = 168;
    let (w, h) = size_for(dpi);
    let dc = CreateCompatibleDC(GetDC(null_mut()));
    let mut info = [0u32; 10];
    info[0] = 40;
    info[1] = w as u32;
    info[2] = (-h) as u32;
    info[3] = 1 | 32 << 16;
    let mut bits: *mut u8 = null_mut();
    let bmp = CreateDIBSection(dc, info.as_ptr().cast(), 0, &mut bits, null_mut(), 0);
    SelectObject(dc, bmp);
    SetBkMode(dc, 1);
    let now = now_min();
    let hour = sample_hour(state_name, now);
    let v = verdict(&hour, now);
    let (s, l) = (state(&v), live(&hour, now));
    let mut board = BLANK_BOARD;
    board.set(targets(s, &l, v.bad), colours(s, &l, v.bad), Instant::now());
    while board.tick() {}
    if flipping {
        board.set(targets(State::Watch, &l, v.bad), colours(s, &l, v.bad), Instant::now());
        for _ in 0..3 { board.tick(); }
    }
    let p = Painter { dc, s: dpi as f32 / 96.0 };
    draw_board(&p, &hour, now, &board, w, h);
    GdiFlush();
    let _ = std::fs::write(out, std::slice::from_raw_parts(bits, (w * h * 4) as usize));
    let _ = std::fs::write(format!("{out}.size"), format!("{w}x{h}"));
}

fn size_for(dpi: u32) -> (i32, i32) {
    let s = dpi as f32 / 96.0;
    ((BW as f32 * s).round() as i32, (BH as f32 * s).round() as i32)
}

fn pos_file() -> std::path::PathBuf {
    data_dir().join("position.txt")
}

const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";

const APPROVED_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\Run";

unsafe fn starts_with_windows() -> bool {
    let name = wide("Netwatch");
    let mut size = 0u32;
    let listed = RegGetValueW(HKCU, wide(RUN_KEY).as_ptr(), name.as_ptr(), RRF_RT_REG_SZ, null_mut(), null_mut(), &mut size) == 0;
    let mut flags = [0u8; 12];
    let mut size = flags.len() as u32;
    let found = RegGetValueW(HKCU, wide(APPROVED_KEY).as_ptr(), name.as_ptr(), RRF_RT_REG_BINARY, null_mut(), flags.as_mut_ptr().cast(), &mut size) == 0;
    listed && !(found && flags[0] & 1 == 1)
}

unsafe fn set_starts_with_windows(on: bool) {
    let name = wide("Netwatch");
    RegDeleteKeyValueW(HKCU, wide(APPROVED_KEY).as_ptr(), name.as_ptr());
    if on {
        let Ok(exe) = std::env::current_exe() else { return };
        let command = wide(&format!("\"{}\"", exe.display()));
        RegSetKeyValueW(HKCU, wide(RUN_KEY).as_ptr(), name.as_ptr(), REG_SZ, command.as_ptr().cast(), (command.len() * 2) as u32);
    } else {
        RegDeleteKeyValueW(HKCU, wide(RUN_KEY).as_ptr(), name.as_ptr());
    }
}

unsafe fn place(hwnd: H) {
    let (w, h) = size_for(GetDpiForWindow(hwnd));
    let saved = std::fs::read_to_string(pos_file()).ok().and_then(|s| {
        let mut it = s.split_whitespace().map(|n| n.parse::<i32>().ok());
        Some((it.next()??, it.next()??))
    });
    let (x, y) = match saved {
        Some((x, y)) if !MonitorFromPoint(Point { x, y }, 0).is_null() => (x, y),
        _ => {
            let mut work: Rect = zeroed();
            SystemParametersInfoW(0x30, 0, (&mut work as *mut Rect).cast(), 0);
            let margin = (16.0 * GetDpiForWindow(hwnd) as f32 / 96.0) as i32;
            (work.r - w - margin, work.b - h - margin)
        }
    };
    SetWindowPos(hwnd, HWND_BOTTOM, x, y, w, h, SWP_NOACTIVATE);
}

unsafe extern "system" fn wndproc(hwnd: H, msg: u32, wp: usize, lp: isize) -> isize {
    match msg {
        WM_ERASEBKGND => 1,
        WM_PAINT => { paint(hwnd); 0 }
        WM_APP => { refresh(hwnd); 0 }
        WM_TIMER => {
            if !BOARD.lock().unwrap().tick() { KillTimer(hwnd, 1); }
            InvalidateRect(hwnd, null(), 0);
            0
        }
        WM_LBUTTONDOWN => {
            ReleaseCapture();
            SendMessageW(hwnd, WM_NCLBUTTONDOWN, HTCAPTION, 0);
            let mut r: Rect = zeroed();
            GetWindowRect(hwnd, &mut r);
            let _ = std::fs::write(pos_file(), format!("{} {}", r.l, r.t));
            0
        }
        WM_RBUTTONUP => {
            let menu = CreatePopupMenu();
            let starts = starts_with_windows();
            AppendMenuW(menu, if starts { MF_CHECKED } else { 0 }, 2, wide("Start With Windows").as_ptr());
            AppendMenuW(menu, MF_SEPARATOR, 0, null());
            AppendMenuW(menu, 0, 1, wide("Close").as_ptr());
            let mut p: Point = zeroed();
            GetCursorPos(&mut p);
            SetForegroundWindow(hwnd);
            match TrackPopupMenu(menu, TPM_RETURNCMD | TPM_RIGHTBUTTON, p.x, p.y, 0, hwnd, null()) {
                1 => { PostMessageW(hwnd, WM_CLOSE, 0, 0); }
                2 => set_starts_with_windows(!starts),
                _ => {}
            }
            DestroyMenu(menu);
            0
        }
        WM_CLOSE => {
            CLOSED.store(true, Relaxed);
            DestroyWindow(hwnd);
            0
        }
        WM_ENDSESSION => { save_hour(); 0 }
        WM_DPICHANGED => {
            let r = &*(lp as *const Rect);
            let (w, h) = size_for((wp >> 16) as u32 & 0xffff);
            SetWindowPos(hwnd, null_mut(), r.l, r.t, w, h, SWP_NOZORDER | SWP_NOACTIVATE);
            0
        }
        WM_DESTROY => {
            save_hour();
            PostQuitMessage(0);
            0
        }
        _ if msg == TASKBAR_CREATED.load(Relaxed) as u32 && msg != 0 => {
            if GetWindow(hwnd, GW_OWNER) != desktop_window() { DestroyWindow(hwnd); }
            0
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

fn wide(s: &str) -> Vec<u16> { s.encode_utf16().chain(Some(0)).collect() }

fn main() {
    let args: Vec<String> = std::env::args().collect();
    unsafe {
        SetProcessDpiAwarenessContext(-4);
        if let Some(i) = args.iter().position(|a| a == "--render") {
            render(&args[i + 1], &args[i + 2], args.iter().any(|a| a == "--flipping"));
            return;
        }
        CreateMutexW(null(), 0, wide("netwatch-widget").as_ptr());
        if GetLastError() == 183 { return }
        let saved = std::fs::read_to_string(data_dir().join("hour.txt")).unwrap_or_default();
        *HOUR.lock().unwrap() = parse_hour(&saved);
        TASKBAR_CREATED.store(RegisterWindowMessageW(wide("TaskbarCreated").as_ptr()) as isize, Relaxed);
        let inst = GetModuleHandleW(null());
        let class = wide("NetwatchWidget");
        RegisterClassW(&WndClass { style: 0, wndproc, cls_extra: 0, wnd_extra: 0, inst, icon: null_mut(),
            cursor: LoadCursorW(null_mut(), 32512), background: null_mut(), menu: null(), class: class.as_ptr() });
        std::thread::spawn(pinger);

        while !CLOSED.load(Relaxed) {
            let desktop = desktop_window();
            if desktop.is_null() {
                std::thread::sleep(Duration::from_secs(1));
                continue;
            }
            let hwnd = CreateWindowExW(WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE, class.as_ptr(), wide("Netwatch").as_ptr(),
                WS_POPUP, 0, 0, 0, 0, desktop, null_mut(), inst, null_mut());
            if hwnd.is_null() {
                std::thread::sleep(Duration::from_secs(1));
                continue;
            }
            WINDOW.store(hwnd as isize, Relaxed);
            let corners: u32 = 3;
            DwmSetWindowAttribute(hwnd, 33, (&corners as *const u32).cast(), 4);
            place(hwnd);
            ShowWindow(hwnd, 4);
            refresh(hwnd);

            let mut msg: Msg = zeroed();
            while GetMessageW(&mut msg, null_mut(), 0, 0) > 0 {
                TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
            if !CLOSED.load(Relaxed) { std::thread::sleep(Duration::from_secs(2)) }
        }
    }
}

unsafe fn desktop_window() -> H {
    FindWindowW(wide("Progman").as_ptr(), null())
}

const WM_DESTROY: u32 = 0x0002;
const WM_ENDSESSION: u32 = 0x0016;
const MF_CHECKED: u32 = 0x8;
const MF_SEPARATOR: u32 = 0x800;
const GW_OWNER: u32 = 4;
const HKCU: H = 0x8000_0001u32 as i32 as isize as H;
const RRF_RT_REG_SZ: u32 = 0x2;
const RRF_RT_REG_BINARY: u32 = 0x8;
const WM_CLOSE: u32 = 0x0010;
const HWND_BOTTOM: H = 1 as H;
const REG_SZ: u32 = 1;
const WM_PAINT: u32 = 0x000F;
const WM_ERASEBKGND: u32 = 0x0014;
const WM_NCLBUTTONDOWN: u32 = 0x00A1;
const WM_TIMER: u32 = 0x0113;
const WM_LBUTTONDOWN: u32 = 0x0201;
const WM_RBUTTONUP: u32 = 0x0205;
const WM_DPICHANGED: u32 = 0x02E0;
const WM_APP: u32 = 0x8000;
const HTCAPTION: usize = 2;
const WS_POPUP: u32 = 0x8000_0000;
const WS_EX_TOOLWINDOW: u32 = 0x80;
const WS_EX_NOACTIVATE: u32 = 0x0800_0000;
const SWP_NOZORDER: u32 = 0x4;
const SWP_NOACTIVATE: u32 = 0x10;
const TPM_RETURNCMD: u32 = 0x100;
const TPM_RIGHTBUTTON: u32 = 0x2;
const LEFT: u32 = 0x0;
const DT_CENTER: u32 = 0x1;
const RIGHT: u32 = 0x2;
const DT_VCENTER: u32 = 0x4;
const DT_SINGLELINE: u32 = 0x20;
const DT_NOPREFIX: u32 = 0x800;
const DT_NOCLIP: u32 = 0x100;

#[repr(C)] struct Point { x: i32, y: i32 }
#[repr(C)] #[derive(Clone, Copy)] struct Rect { l: i32, t: i32, r: i32, b: i32 }
#[repr(C)] struct Msg { hwnd: H, message: u32, wp: usize, lp: isize, time: u32, pt: Point, private: u32 }
#[repr(C)] struct PaintStruct { dc: H, erase: i32, rc: Rect, restore: i32, inc: i32, reserved: [u8; 32] }
#[repr(C)] struct WndClass {
    style: u32, wndproc: unsafe extern "system" fn(H, u32, usize, isize) -> isize, cls_extra: i32, wnd_extra: i32,
    inst: H, icon: H, cursor: H, background: H, menu: *const u16, class: *const u16,
}

#[link(name = "user32")]
extern "system" {
    fn RegisterClassW(c: *const WndClass) -> u16;
    fn CreateWindowExW(ex: u32, class: *const u16, name: *const u16, style: u32, x: i32, y: i32, w: i32, h: i32, parent: H, menu: H, inst: H, param: *mut c_void) -> H;
    fn DefWindowProcW(h: H, m: u32, w: usize, l: isize) -> isize;
    fn GetMessageW(m: *mut Msg, h: H, a: u32, b: u32) -> i32;
    fn TranslateMessage(m: *const Msg) -> i32;
    fn DispatchMessageW(m: *const Msg) -> isize;
    fn PostQuitMessage(code: i32);
    fn PostMessageW(h: H, m: u32, w: usize, l: isize) -> i32;
    fn SendMessageW(h: H, m: u32, w: usize, l: isize) -> isize;
    fn BeginPaint(h: H, ps: *mut PaintStruct) -> H;
    fn EndPaint(h: H, ps: *const PaintStruct) -> i32;
    fn FillRect(dc: H, r: *const Rect, brush: H) -> i32;
    fn DrawTextW(dc: H, text: *const u16, len: i32, r: *mut Rect, flags: u32) -> i32;
    fn GetClientRect(h: H, r: *mut Rect) -> i32;
    fn GetDC(h: H) -> H;
    fn InvalidateRect(h: H, r: *const Rect, erase: i32) -> i32;
    fn FindWindowW(class: *const u16, name: *const u16) -> H;
    fn SetWindowPos(h: H, after: H, x: i32, y: i32, cx: i32, cy: i32, flags: u32) -> i32;
    fn GetWindowRect(h: H, r: *mut Rect) -> i32;
    fn ShowWindow(h: H, cmd: i32) -> i32;
    fn DestroyWindow(h: H) -> i32;
    fn SetTimer(h: H, id: usize, ms: u32, f: *const c_void) -> usize;
    fn KillTimer(h: H, id: usize) -> i32;
    fn SystemParametersInfoW(action: u32, p1: u32, p2: *mut c_void, flags: u32) -> i32;
    fn MonitorFromPoint(p: Point, flags: u32) -> H;
    fn GetDpiForWindow(h: H) -> u32;
    fn SetProcessDpiAwarenessContext(ctx: isize) -> i32;
    fn ReleaseCapture() -> i32;
    fn LoadCursorW(inst: H, name: usize) -> H;
    fn GetCursorPos(p: *mut Point) -> i32;
    fn SetForegroundWindow(h: H) -> i32;
    fn CreatePopupMenu() -> H;
    fn AppendMenuW(m: H, flags: u32, id: usize, text: *const u16) -> i32;
    fn TrackPopupMenu(m: H, flags: u32, x: i32, y: i32, r: i32, h: H, rc: *const Rect) -> i32;
    fn DestroyMenu(m: H) -> i32;
    fn GetWindow(h: H, cmd: u32) -> H;
    fn RegisterWindowMessageW(name: *const u16) -> u32;
}
#[link(name = "advapi32")]
extern "system" {
    fn RegGetValueW(key: H, sub: *const u16, value: *const u16, flags: u32, kind: *mut u32, data: *mut c_void, size: *mut u32) -> i32;
    fn RegSetKeyValueW(key: H, sub: *const u16, value: *const u16, kind: u32, data: *const c_void, size: u32) -> i32;
    fn RegDeleteKeyValueW(key: H, sub: *const u16, value: *const u16) -> i32;
}
#[link(name = "gdi32")]
extern "system" {
    fn CreateSolidBrush(colour: u32) -> H;
    fn DeleteObject(o: H) -> i32;
    fn CreateCompatibleDC(dc: H) -> H;
    fn CreateCompatibleBitmap(dc: H, w: i32, h: i32) -> H;
    fn CreateDIBSection(dc: H, info: *const c_void, usage: u32, bits: *mut *mut u8, section: H, offset: u32) -> H;
    fn SelectObject(dc: H, o: H) -> H;
    fn GetStockObject(i: i32) -> H;
    fn BitBlt(dst: H, x: i32, y: i32, w: i32, h: i32, src: H, sx: i32, sy: i32, rop: u32) -> i32;
    fn DeleteDC(dc: H) -> i32;
    fn GdiFlush() -> i32;
    fn SaveDC(dc: H) -> i32;
    fn RestoreDC(dc: H, n: i32) -> i32;
    fn IntersectClipRect(dc: H, l: i32, t: i32, r: i32, b: i32) -> i32;
    fn Ellipse(dc: H, l: i32, t: i32, r: i32, b: i32) -> i32;
    fn SetBkMode(dc: H, mode: i32) -> i32;
    fn SetTextColor(dc: H, colour: u32) -> u32;
    fn SetTextCharacterExtra(dc: H, extra: i32) -> i32;
    fn CreateFontW(h: i32, w: i32, esc: i32, orient: i32, weight: i32, italic: u32, underline: u32, strike: u32,
        charset: u32, out_prec: u32, clip_prec: u32, quality: u32, pitch: u32, face: *const u16) -> H;
}
#[link(name = "kernel32")]
extern "system" {
    fn CreateMutexW(attrs: *const c_void, owner: i32, name: *const u16) -> H;
    fn GetLastError() -> u32;
    fn GetModuleHandleW(name: *const u16) -> H;
}
#[link(name = "iphlpapi")]
extern "system" {
    fn IcmpCreateFile() -> H;
    fn IcmpSendEcho(h: H, dest: u32, data: *const c_void, size: u16, opts: *const c_void, reply: *mut c_void, reply_size: u32, timeout: u32) -> u32;
    fn GetBestRoute(dest: u32, source: u32, row: *mut u32) -> u32;
}
#[link(name = "dwmapi")]
extern "system" {
    fn DwmSetWindowAttribute(h: H, attr: u32, value: *const c_void, size: u32) -> i32;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn minute(stamp: u64, sent: u8, lost: [u8; 2]) -> Min {
        Min { stamp, sent: [sent, sent, sent], lost: [lost[0], lost[1], 0], rtt: [30 * sent as u32, 30 * sent as u32, 3 * sent as u32] }
    }

    #[test]
    fn verdicts() {
        let now = 1000;
        let mut hour = [EMPTY; 60];
        for i in 0..60 { hour[((now - i) % 60) as usize] = minute(now - i, 60, [0, 0]) }
        assert_eq!(verdict(&hour, now), Verdict { bad: 0, lossy: 0, measured: 60, recent_bad: false });
        assert_eq!(state(&verdict(&hour, now)), State::Clean);

        hour[(now % 60) as usize] = minute(now, 60, [9, 0]);
        assert_eq!(verdict(&hour, now).lossy, 0);

        for i in 0..3 { hour[((now - i) % 60) as usize] = minute(now - i, 60, [9, 8]) }
        let v = verdict(&hour, now);
        assert_eq!(v.bad, 3);
        assert_eq!(state(&v), State::Reset);

        let mut later = hour;
        for i in 1..=10 { later[((now + i) % 60) as usize] = minute(now + i, 60, [0, 0]) }
        let v = verdict(&later, now + 10);
        assert!(v.bad == 3 && state(&v) == State::Recovering);

        assert_eq!(state(&verdict(&hour, now + 12)), State::Reset);

        assert_eq!(verdict(&hour, now + 60).measured, 0);

        let one = Min { stamp: now, sent: [1, 0, 0], lost: [1, 0, 0], rtt: [0; 3] };
        assert_eq!(one.lost(), 0);

        let weak = Min { stamp: now, sent: [60; 3], lost: [6, 6, 5], rtt: [0; 3] };
        assert_eq!(weak.lost(), 1);
        let hung = Min { stamp: now, sent: [60; 3], lost: [60, 60, 60], rtt: [0; 3] };
        assert_eq!(hung.lost(), 60);
    }

    #[test]
    fn live_readings() {
        let now = 1000;
        let mut hour = [EMPTY; 60];
        for i in 0..5 { hour[((now - i) % 60) as usize] = minute(now - i, 60, [6, 6]) }
        let l = live(&hour, now);
        assert_eq!(l.loss_pct, Some(10.0));
        assert_eq!(l.ping_ms, Some(30 * 60 / 54));
        assert_eq!((l.router_ms, l.router_dropping), (Some(3), false));
        assert_eq!(live(&[EMPTY; 60], now), Live { loss_pct: None, ping_ms: None, router_ms: None, router_dropping: false });
    }

    #[test]
    fn board() {
        let l = Live { loss_pct: Some(13.4), ping_ms: Some(69), router_ms: Some(4), router_dropping: false };
        let t = targets(State::Reset, &l, 20);
        assert_eq!(t.iter().collect::<String>(), "RESET     13% 69  420");
        let calm = Live { loss_pct: Some(0.4), ..l };
        assert_eq!(targets(State::Clean, &calm, 0).iter().collect::<String>(), "CLEAN    0.4% 69  4 0");

        let edge = Live { loss_pct: Some(9.96), ..l };
        assert_eq!(targets(State::Watch, &edge, 1).iter().collect::<String>(), "WATCH     10% 69  4 1");
        assert_eq!(field_colour(LOSS, State::Watch, &edge, 1), RED);
        assert_eq!(targets(State::Reset, &Live { loss_pct: Some(99.7), ..l }, 3)[LOSS.0..LOSS.0 + 4], ['1', '0', '0', '%']);

        let mut b = BLANK_BOARD;
        let start = Instant::now();
        b.set(t, colours(State::Reset, &l, 20), start);
        let mut frames = 0;
        while b.tick() { frames += 1 }
        assert_eq!(b.shown, t);
        assert!(frames < 60, "settled in {frames} frames");

        let fine = Live { ping_ms: Some(70), loss_pct: Some(1.0), ..l };
        let next = targets(State::Watch, &fine, 20);
        b.set(next, colours(State::Watch, &fine, 20), start + Duration::from_secs(3));
        assert_eq!((b.target[0], b.colour[0]), ('W', AMBER));
        assert_eq!((b.target[PING.0 + 2], b.colour[1]), ('9', RED));
        b.set(next, colours(State::Watch, &fine, 20), start + NUMBER_REFRESH);
        assert_eq!((b.target[PING.0 + 2], b.colour[1]), ('0', LETTER));
    }

    #[test]
    fn saved_hour() {
        let mut hour = [EMPTY; 60];
        hour[1000 % 60] = minute(1000, 60, [9, 8]);
        hour[999 % 60] = minute(999, 41, [0, 2]);
        let back = parse_hour(&hour_text(&hour));
        assert_eq!(hour_text(&back), hour_text(&hour));
        assert_eq!(back[1000 % 60].lost(), 8);

        let text = "1000 60 60 60 9 8 0 1530 1560 180\n1001 5 5 5 9 0 0 1 1 1\n1002 60 60\n1003 60 x 60 0 0 0 1 1 1\n";
        let hour = parse_hour(text);
        assert_eq!(hour.iter().filter(|m| m.stamp > 0).count(), 1);
    }
}
