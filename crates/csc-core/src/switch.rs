#![cfg_attr(not(windows), allow(dead_code))]

use crate::layout::ScreenLayout;
use csc_config::SwitchConfig;
use csc_platform::{Rect, ScreenSize};
use csc_proto::Edge;
use std::time::Instant;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SwitchState {
    Local,
    Remote,
    #[allow(dead_code)]
    Locked,
}

#[derive(Debug, Clone)]
pub struct EnterDecision {
    pub neighbor: String,
    pub edge: Edge,
    pub position_pct: f32,
}

/// Reset when the pointer leaves an outer edge. The outer pixel itself
/// switches; there is no extra push past it.
#[derive(Debug, Clone, Copy)]
pub struct EdgePush;

fn pixel_on_edge(edge: Edge, x: i32, y: i32, w: i32, h: i32) -> bool {
    match edge {
        Edge::Left => x <= 0,
        Edge::Right => x >= w - 1,
        Edge::Top => y <= 0,
        Edge::Bottom => y >= h - 1,
        Edge::TopLeft => x <= 0 && y <= 0,
        Edge::TopRight => x >= w - 1 && y <= 0,
        Edge::BottomLeft => x <= 0 && y >= h - 1,
        Edge::BottomRight => x >= w - 1 && y >= h - 1,
    }
}

fn outward(edge: Edge, dx: i32, dy: i32) -> i32 {
    match edge {
        Edge::Left => -dx,
        Edge::Right => dx,
        Edge::Top => -dy,
        Edge::Bottom => dy,
        Edge::TopLeft => (-dx).max(-dy),
        Edge::TopRight => dx.max(-dy),
        Edge::BottomLeft => (-dx).max(dy),
        Edge::BottomRight => dx.max(dy),
    }
}

impl EdgePush {
    pub fn reset(&mut self) {}
}

/// An outer edge the real pointer of this (controlled) computer pushed through.
#[derive(Debug, Clone, PartialEq)]
pub struct EdgeReport {
    pub edge: Edge,
    /// Corner zone the pointer is in, if any; the host prefers it when the layout has one.
    pub corner: Option<Edge>,
    pub position_pct: f32,
}

pub struct SwitchController {
    local_name: String,
    screen: ScreenSize,
    /// Local monitors. Empty means one monitor covering `screen`.
    monitors: Vec<Rect>,
    /// Area of each monitor that is not a taskbar. Same order as `monitors`.
    /// Empty, or a different length, means the whole monitor.
    work: Vec<Rect>,
    cfg: SwitchConfig,
    layout: ScreenLayout,
    state: SwitchState,
    /// Host currently receiving the pointer, when `state` is remote.
    active_remote: String,
    enabled: bool,
    /// A held modifier, or “stay on this screen”, is blocking an edge crossing.
    blocked: bool,
    pending_edge: Option<Edge>,
    pending_since: Option<Instant>,
    pending_second_tap: bool,
    push: EdgePush,
    last_local: Option<(i32, i32)>,
    /// Consecutive steps where the cursor moved but raw motion stayed silent.
    /// Silent raw still switches; it only stops raw from picking a corner side.
    raw_misses: u8,
    /// The active remote reports its own edge contact (EdgeHit).
    remote_reports: bool,
}

/// Raw motion that missed this many cursor movements in a row is not used
/// to choose which side a corner press is aimed at.
const RAW_MISS_LIMIT: u8 = 3;

impl Default for SwitchController {
    fn default() -> Self {
        Self {
            local_name: String::new(),
            screen: ScreenSize::default(),
            monitors: Vec::new(),
            work: Vec::new(),
            cfg: SwitchConfig::default(),
            layout: ScreenLayout::default(),
            state: SwitchState::Local,
            active_remote: String::new(),
            enabled: true,
            blocked: false,
            pending_edge: None,
            pending_since: None,
            pending_second_tap: false,
            push: EdgePush,
            last_local: None,
            raw_misses: RAW_MISS_LIMIT,
            remote_reports: false,
        }
    }
}

impl SwitchController {
    pub fn configure(
        &mut self,
        local_name: String,
        screen: ScreenSize,
        cfg: SwitchConfig,
        layout: ScreenLayout,
    ) {
        self.local_name = local_name;
        self.screen = screen;
        self.cfg = cfg;
        self.layout = layout;
    }

    pub fn set_screen(&mut self, screen: ScreenSize, monitors: Vec<Rect>) {
        self.screen = screen;
        self.monitors = monitors;
    }

    /// Work areas aligned with `set_screen`'s monitor list. A point on the
    /// taskbar is still on this computer, so it does not switch.
    pub fn set_work_areas(&mut self, areas: Vec<Rect>) {
        self.work = areas;
    }

    fn work_for(&self, monitor: &Rect) -> Rect {
        let list = self.monitor_list();
        if self.work.len() != list.len() {
            return *monitor;
        }
        let work = list
            .iter()
            .zip(self.work.iter())
            .find(|(m, _)| **m == *monitor)
            .map(|(_, work)| *work)
            .unwrap_or(*monitor);
        // A work rect that misses this monitor is not its taskbar.
        let overlaps = work.left < monitor.right
            && monitor.left < work.right
            && work.top < monitor.bottom
            && monitor.top < work.bottom;
        if overlaps {
            work
        } else {
            *monitor
        }
    }

    /// Pointer is on this computer's taskbar or panel, not past the screen.
    fn on_shell_bar(&self, x: i32, y: i32) -> bool {
        let monitor = self.monitor_at(x, y);
        monitor.contains(x, y) && !self.work_for(&monitor).contains(x, y)
    }

    /// Outer pixel of a side that leads to another computer. The taskbar covers
    /// that pixel, but it is still the way off this screen.
    fn shell_edge_leads(&self, x: i32, y: i32) -> bool {
        let leads = |e: Edge| self.layout.neighbor(&self.local_name, e).is_some();
        if self.diagonal_at(x, y).is_some_and(leads) {
            return true;
        }
        self.touched_outer_edges(x, y).into_iter().any(leads)
    }

    fn monitor_list(&self) -> Vec<Rect> {
        if self.monitors.is_empty() {
            vec![Rect {
                left: 0,
                top: 0,
                right: self.screen.width,
                bottom: self.screen.height,
            }]
        } else {
            self.monitors.clone()
        }
    }

    /// Monitor holding the point, or the nearest one when the point is in a gap.
    pub fn monitor_at(&self, x: i32, y: i32) -> Rect {
        let list = self.monitor_list();
        if let Some(m) = list.iter().find(|m| m.contains(x, y)) {
            return *m;
        }
        let dist = |m: &Rect| {
            let dx = (m.left - x).max(0).max(x - (m.right - 1));
            let dy = (m.top - y).max(0).max(y - (m.bottom - 1));
            dx as i64 * dx as i64 + dy as i64 * dy as i64
        };
        list.into_iter().min_by_key(dist).unwrap_or_default()
    }

    /// Sides of monitor `m` that lie on the outer boundary of the whole desktop.
    /// A border between two monitors, or the side of a monitor that is set back
    /// from its neighbors, never switches computers.
    fn outer_sides(&self, m: &Rect) -> (bool, bool, bool, bool) {
        let list = self.monitor_list();
        let min_left = list.iter().map(|r| r.left).min().unwrap_or(m.left);
        let max_right = list.iter().map(|r| r.right).max().unwrap_or(m.right);
        let min_top = list.iter().map(|r| r.top).min().unwrap_or(m.top);
        let max_bottom = list.iter().map(|r| r.bottom).max().unwrap_or(m.bottom);
        (
            m.left <= min_left,
            m.right >= max_right,
            m.top <= min_top,
            m.bottom >= max_bottom,
        )
    }

    pub fn state(&self) -> SwitchState {
        self.state
    }

    pub fn set_enabled(&mut self, enabled: bool) {
        self.enabled = enabled;
    }

    pub fn set_blocked(&mut self, blocked: bool) {
        if blocked && !self.blocked {
            self.push.reset();
            self.pending_edge = None;
        }
        self.blocked = blocked;
    }

    pub fn enabled(&self) -> bool {
        self.enabled
    }

    pub fn unlock_to_local(&mut self) {
        self.state = SwitchState::Local;
        self.active_remote.clear();
        self.pending_edge = None;
        self.pending_since = None;
        self.pending_second_tap = false;
        self.push.reset();
        self.last_local = None;
        self.remote_reports = false;
    }

    /// The active remote judges its own edges and sends EdgeHit; the host
    /// then only tracks an estimate of the remote pointer for entry positions.
    pub fn set_remote_reports(&mut self, on: bool) {
        self.remote_reports = on;
    }

    /// Keyboard host: the active remote pushed through `edge`. Picks the side
    /// the pointer leaves through, as `on_remote_relative` would.
    pub fn on_remote_edge(&self, report: &EdgeReport) -> Option<EnterDecision> {
        if self.state != SwitchState::Remote {
            return None;
        }
        let leads = |e: Edge| self.layout.neighbor(&self.active_remote, e).is_some();
        let way_back = self
            .layout
            .edge_toward(&self.active_remote, &self.local_name)
            .is_some();
        let edge = match report.corner.filter(|c| leads(*c)) {
            Some(c) => c,
            None if leads(report.edge) => report.edge,
            // No link back to this computer: any side returns, so the pointer cannot get stuck.
            None if !way_back && !is_corner(report.edge) => report.edge,
            None => return None,
        };
        Some(EnterDecision {
            neighbor: self.local_name.clone(),
            edge,
            position_pct: report.position_pct.clamp(0.0, 1.0),
        })
    }

    /// Controlled computer: the real pointer is at (x, y) after moving by
    /// `moved`, while the host sent `raw`. The outer pixel is shared with the
    /// neighbor, so occupying it is not a leave. A move that pushes outward
    /// off that pixel is. Any outer edge counts; the keyboard host knows
    /// which ones lead somewhere.
    pub fn probe_edge(
        &self,
        push: &mut EdgePush,
        x: i32,
        y: i32,
        raw: (i32, i32),
        _moved: (i32, i32),
    ) -> Option<EdgeReport> {
        let Some(edge) = pick_edge(&self.touched_outer_edges(x, y), raw, |_| true) else {
            push.reset();
            return None;
        };
        let corner = self.diagonal_at(x, y);
        // Sliding along the shared pixel is still movement. Only a push
        // outward, off that pixel, leaves this screen.
        if outward(edge, raw.0, raw.1) <= 0 {
            return None;
        }
        push.reset();
        let d = self.make_decision(String::new(), edge, x, y);
        Some(EdgeReport {
            edge,
            corner,
            position_pct: d.position_pct,
        })
    }

    pub fn active_remote(&self) -> &str {
        &self.active_remote
    }

    /// The pointer is on an outer pixel that is also the facing pixel of a
    /// neighbor. Bottom pixel 0 of one screen and the other screen's last
    /// pixel (for a 1024-tall screen, y = 0x3FF) are that same pixel.
    pub fn cursor_on_shared_pixel(&self, x: i32, y: i32, remote: ScreenSize) -> bool {
        if self.active_remote.is_empty() {
            return false;
        }
        let w = remote.width.max(1);
        let h = remote.height.max(1);
        [
            Edge::Left,
            Edge::Right,
            Edge::Top,
            Edge::Bottom,
            Edge::TopLeft,
            Edge::TopRight,
            Edge::BottomLeft,
            Edge::BottomRight,
        ]
        .into_iter()
        .any(|edge| {
            self.layout.neighbor(&self.active_remote, edge).is_some()
                && pixel_on_edge(edge, x, y, w, h)
        })
    }

    /// Neighbor of the active remote screen on `edge`, if the layout has one.
    /// Sides of this screen (left, right, top, bottom) that lead to a neighbor.
    /// A corner neighbor needs both sides that meet there.
    pub fn local_sides(&self) -> (bool, bool, bool, bool) {
        let leads = |e: Edge| self.layout.neighbor(&self.local_name, e).is_some();
        let tl = leads(Edge::TopLeft);
        let tr = leads(Edge::TopRight);
        let bl = leads(Edge::BottomLeft);
        let br = leads(Edge::BottomRight);
        (
            leads(Edge::Left) || tl || bl,
            leads(Edge::Right) || tr || br,
            leads(Edge::Top) || tl || tr,
            leads(Edge::Bottom) || bl || br,
        )
    }

    pub fn neighbor_from_active(&self, edge: Edge) -> Option<String> {
        if self.active_remote.is_empty() {
            return None;
        }
        self.layout
            .neighbor(&self.active_remote, edge)
            .map(|s| s.to_string())
    }

    pub fn confirm_entered(
        &mut self,
        entry_edge: Edge,
        position_pct: f32,
        remote: ScreenSize,
        cursor_x: &mut i32,
        cursor_y: &mut i32,
        remote_host: &str,
    ) {
        self.state = SwitchState::Remote;
        if !remote_host.is_empty() {
            self.active_remote = remote_host.to_string();
        }
        let pct = position_pct.clamp(0.0, 1.0);
        // This computer is being entered: map onto the monitors that form the
        // edge. A remote peer is only a width and height, so the host keeps a
        // bounding-box estimate and the controlled computer places the pointer.
        if remote.width == self.screen.width && remote.height == self.screen.height {
            let (x, y) = self.place_on_edge(entry_edge, pct);
            *cursor_x = x;
            *cursor_y = y;
            return;
        }
        match entry_edge {
            Edge::Left => {
                *cursor_x = 0;
                *cursor_y = ((remote.height as f32) * pct) as i32;
            }
            Edge::Right => {
                *cursor_x = remote.width.saturating_sub(1);
                *cursor_y = ((remote.height as f32) * pct) as i32;
            }
            Edge::Top => {
                *cursor_y = 0;
                *cursor_x = ((remote.width as f32) * pct) as i32;
            }
            Edge::Bottom => {
                *cursor_y = remote.height.saturating_sub(1);
                *cursor_x = ((remote.width as f32) * pct) as i32;
            }
            Edge::TopLeft => {
                *cursor_x = 0;
                *cursor_y = 0;
            }
            Edge::TopRight => {
                *cursor_x = remote.width.saturating_sub(1);
                *cursor_y = 0;
            }
            Edge::BottomLeft => {
                *cursor_x = 0;
                *cursor_y = remote.height.saturating_sub(1);
            }
            Edge::BottomRight => {
                *cursor_x = remote.width.saturating_sub(1);
                *cursor_y = remote.height.saturating_sub(1);
            }
        }
    }

    /// Where `pct` (0 at the top or left) lands on this computer's outer edge.
    /// The percentage runs along the monitors that actually form that side,
    /// skipping gaps. It does not use the bounding box, so a shorter monitor
    /// beside a taller one keeps the same fraction of its own edge.
    pub fn place_on_edge(&self, edge: Edge, pct: f32) -> (i32, i32) {
        let pct = pct.clamp(0.0, 1.0);
        if is_corner(edge) {
            return self.corner_pixel(edge);
        }
        let spans = self.edge_spans(edge);
        let total: i32 = spans.iter().map(|(from, to, _)| to - from).sum();
        if total <= 0 {
            return self.box_point(edge, pct);
        }
        let mut cursor = ((total as f32) * pct).round() as i32;
        cursor = cursor.clamp(0, total - 1);
        for (from, to, at) in spans {
            let len = to - from;
            if cursor < len {
                let along = from + cursor;
                return match edge {
                    Edge::Left | Edge::Right => (at, along),
                    _ => (along, at),
                };
            }
            cursor -= len;
        }
        self.box_point(edge, pct)
    }

    /// Pointer back on the local side opposite `remote_edge`, one pixel in
    /// so the crossing itself does not immediately switch again.
    pub fn local_reentry(&self, remote_edge: Edge, pct: f32) -> (i32, i32) {
        let local = remote_edge.opposite();
        let (mut x, mut y) = self.place_on_edge(local, pct);
        let m = self.monitor_at(x, y);
        const INSET: i32 = 1;
        match local {
            Edge::Left | Edge::TopLeft | Edge::BottomLeft => {
                x = (x + INSET).min(m.right - 1);
            }
            Edge::Right | Edge::TopRight | Edge::BottomRight => {
                x = (x - INSET).max(m.left);
            }
            _ => {}
        }
        match local {
            Edge::Top | Edge::TopLeft | Edge::TopRight => {
                y = (y + INSET).min(m.bottom - 1);
            }
            Edge::Bottom | Edge::BottomLeft | Edge::BottomRight => {
                y = (y - INSET).max(m.top);
            }
            _ => {}
        }
        (x, y)
    }

    /// Bounding-box point. Used when this computer has no monitor geometry.
    fn box_point(&self, edge: Edge, pct: f32) -> (i32, i32) {
        let w = self.screen.width.max(1);
        let h = self.screen.height.max(1);
        let pct = pct.clamp(0.0, 1.0);
        match edge {
            Edge::Left => (0, ((h as f32) * pct) as i32),
            Edge::Right => (w - 1, ((h as f32) * pct) as i32),
            Edge::Top => (((w as f32) * pct) as i32, 0),
            Edge::Bottom => (((w as f32) * pct) as i32, h - 1),
            Edge::TopLeft => (0, 0),
            Edge::TopRight => (w - 1, 0),
            Edge::BottomLeft => (0, h - 1),
            Edge::BottomRight => (w - 1, h - 1),
        }
    }

    fn corner_pixel(&self, edge: Edge) -> (i32, i32) {
        let list = self.monitor_list();
        let (min_left, max_right, min_top, max_bottom) = desk_bounds(&list);
        match edge {
            Edge::TopLeft => list
                .iter()
                .find(|m| m.left <= min_left && m.top <= min_top)
                .map(|m| (m.left, m.top))
                .unwrap_or((min_left, min_top)),
            Edge::TopRight => list
                .iter()
                .find(|m| m.right >= max_right && m.top <= min_top)
                .map(|m| (m.right - 1, m.top))
                .unwrap_or((max_right.saturating_sub(1), min_top)),
            Edge::BottomLeft => list
                .iter()
                .find(|m| m.left <= min_left && m.bottom >= max_bottom)
                .map(|m| (m.left, m.bottom - 1))
                .unwrap_or((min_left, max_bottom.saturating_sub(1))),
            Edge::BottomRight => list
                .iter()
                .find(|m| m.right >= max_right && m.bottom >= max_bottom)
                .map(|m| (m.right - 1, m.bottom - 1))
                .unwrap_or((max_right.saturating_sub(1), max_bottom.saturating_sub(1))),
            _ => self.box_point(edge, 0.5),
        }
    }

    /// Exposed pieces of one outer side, `(from, to, at)` in along-edge order.
    /// `from..to` is the span; `at` is the outer pixel. Gaps are not included.
    fn edge_spans(&self, edge: Edge) -> Vec<(i32, i32, i32)> {
        let list = self.monitor_list();
        let (min_left, max_right, min_top, max_bottom) = desk_bounds(&list);
        let mut spans = Vec::new();
        for m in &list {
            let (from, to, at, exposed) = match edge {
                Edge::Left => (m.top, m.bottom, m.left, m.left <= min_left),
                Edge::Right => (m.top, m.bottom, m.right - 1, m.right >= max_right),
                Edge::Top => (m.left, m.right, m.top, m.top <= min_top),
                Edge::Bottom => (m.left, m.right, m.bottom - 1, m.bottom >= max_bottom),
                _ => continue,
            };
            if exposed && to > from {
                spans.push((from, to, at));
            }
        }
        spans.sort_by_key(|span| (span.0, span.2));
        let mut merged: Vec<(i32, i32, i32)> = Vec::new();
        for (from, to, at) in spans {
            if let Some(last) = merged.last_mut() {
                if last.2 == at && from <= last.1 {
                    last.1 = last.1.max(to);
                    continue;
                }
            }
            merged.push((from, to, at));
        }
        merged
    }

    /// Fraction along the outer side, 0 at the top or the left. Segments are
    /// joined in order and gaps between them are skipped.
    fn fraction_on_edge(&self, edge: Edge, x: i32, y: i32) -> f32 {
        if is_corner(edge) {
            return 0.5;
        }
        let along = match edge {
            Edge::Left | Edge::Right => y,
            _ => x,
        };
        let spans = self.edge_spans(edge);
        let total: i32 = spans.iter().map(|(from, to, _)| to - from).sum();
        if total <= 0 {
            return self.monitor_fraction(edge, x, y);
        }
        let mut acc = 0i32;
        for (from, to, _) in spans {
            if along >= from && along < to {
                return ((acc + (along - from)) as f32 / total as f32).clamp(0.0, 1.0);
            }
            acc += to - from;
        }
        self.monitor_fraction(edge, x, y)
    }

    fn monitor_fraction(&self, edge: Edge, x: i32, y: i32) -> f32 {
        let m = self.monitor_at(x, y);
        let pct = match edge {
            Edge::Left | Edge::Right if m.height() > 0 => (y - m.top) as f32 / m.height() as f32,
            Edge::Top | Edge::Bottom if m.width() > 0 => (x - m.left) as f32 / m.width() as f32,
            _ => 0.5,
        };
        pct.clamp(0.0, 1.0)
    }

    pub fn confirm_returned(&mut self) {
        self.unlock_to_local();
    }

    /// Outer sides the point touches: two at a corner pixel.
    fn touched_outer_edges(&self, x: i32, y: i32) -> Vec<Edge> {
        let m = self.monitor_at(x, y);
        let (ol, or, ot, ob) = self.outer_sides(&m);
        let mut out = Vec::with_capacity(2);
        if ol && x <= m.left {
            out.push(Edge::Left);
        } else if or && x >= m.right - 1 {
            out.push(Edge::Right);
        }
        if ot && y <= m.top {
            out.push(Edge::Top);
        } else if ob && y >= m.bottom - 1 {
            out.push(Edge::Bottom);
        }
        out
    }

    /// Distance from a corner, along a side, that counts as that corner.
    fn diagonal_zone(&self, side_len: i32) -> i32 {
        let z = self
            .cfg
            .diagonal_zone_px
            .max(self.cfg.corner_dead_zone_px)
            .max(1);
        z.min((side_len / 3).max(1))
    }

    /// Corner zone the pointer is pressing into: it must touch an outer side
    /// and be within the diagonal zone of one end of that side.
    fn diagonal_at(&self, x: i32, y: i32) -> Option<Edge> {
        let m = self.monitor_at(x, y);
        let (ol, or, ot, ob) = self.outer_sides(&m);
        let zy = self.diagonal_zone(m.height());
        let zx = self.diagonal_zone(m.width());
        let on_left = ol && x <= m.left;
        let on_right = or && x >= m.right - 1;
        let on_top = ot && y <= m.top;
        let on_bottom = ob && y >= m.bottom - 1;
        let others: Vec<Rect> = self
            .monitor_list()
            .into_iter()
            .filter(|o| *o != m)
            .collect();
        let near_top = y <= m.top + zy && !others.iter().any(|o| o.bottom <= m.top);
        let near_bottom = y >= m.bottom - 1 - zy && !others.iter().any(|o| o.top >= m.bottom);
        let near_left = x <= m.left + zx && !others.iter().any(|o| o.right <= m.left);
        let near_right = x >= m.right - 1 - zx && !others.iter().any(|o| o.left >= m.right);
        if (on_left && near_top) || (on_top && near_left) {
            Some(Edge::TopLeft)
        } else if (on_right && near_top) || (on_top && near_right) {
            Some(Edge::TopRight)
        } else if (on_left && near_bottom) || (on_bottom && near_left) {
            Some(Edge::BottomLeft)
        } else if (on_right && near_bottom) || (on_bottom && near_right) {
            Some(Edge::BottomRight)
        } else {
            None
        }
    }

    /// Local pointer at (x, y). `raw` is the hand movement since the last call
    /// when the platform can measure it. It only chooses a side at a corner.
    /// The outer pixel switches either way.
    fn usable_raw(&mut self, raw: Option<(i32, i32)>, moved: (i32, i32)) -> Option<(i32, i32)> {
        let r = raw?;
        if moved != (0, 0) {
            let trusted = self.raw_misses < RAW_MISS_LIMIT;
            self.raw_misses = if r == (0, 0) {
                self.raw_misses.saturating_add(1)
            } else {
                0
            };
            let now_trusted = self.raw_misses < RAW_MISS_LIMIT;
            if trusted != now_trusted {
                if now_trusted {
                    tracing::info!("raw mouse motion available");
                } else {
                    tracing::info!("raw mouse motion silent while the cursor moves");
                }
            }
        }
        (self.raw_misses < RAW_MISS_LIMIT).then_some(r)
    }

    pub fn on_local_cursor(
        &mut self,
        x: i32,
        y: i32,
        raw: Option<(i32, i32)>,
        now: Instant,
    ) -> Option<EnterDecision> {
        if !self.enabled || self.blocked || self.state != SwitchState::Local {
            return None;
        }
        let moved = self
            .last_local
            .map(|(lx, ly)| (x - lx, y - ly))
            .unwrap_or((0, 0));
        self.last_local = Some((x, y));
        // Inside the taskbar, but not on an outer edge that leads somewhere.
        // That edge is how the pointer reaches the other computer.
        if self.on_shell_bar(x, y) && !self.shell_edge_leads(x, y) {
            self.pending_edge = None;
            self.pending_second_tap = false;
            self.push.reset();
            return None;
        }
        let raw = self.usable_raw(raw, moved);
        let diagonal = self
            .diagonal_at(x, y)
            .filter(|d| self.layout.neighbor(&self.local_name, *d).is_some());
        let touched = self.touched_outer_edges(x, y);
        let leads = |e: Edge| self.layout.neighbor(&self.local_name, e).is_some();
        let edge = if let Some(diag) = diagonal {
            diag
        } else if let Some(edge) =
            pick_edge(&touched, raw.unwrap_or((0, 0)), leads).or_else(|| touched.first().copied())
        {
            edge
        } else {
            self.pending_edge = None;
            self.pending_second_tap = false;
            self.push.reset();
            return None;
        };
        let neighbor = self.layout.neighbor(&self.local_name, edge)?.to_string();
        // The cursor is on the outer pixel. That pixel is the other computer.
        self.push.reset();

        if self.cfg.double_tap_edge {
            if self.pending_edge == Some(edge) && self.pending_second_tap {
                let since = self.pending_since.unwrap_or(now);
                if now.duration_since(since).as_millis() as i32 <= self.cfg.double_tap_window_ms {
                    self.pending_edge = None;
                    self.pending_second_tap = false;
                    return Some(self.make_decision(neighbor, edge, x, y));
                }
            }
            if self.pending_edge != Some(edge) {
                self.pending_edge = Some(edge);
                self.pending_since = Some(now);
                self.pending_second_tap = false;
            } else if !self.pending_second_tap {
                // left edge then came back conceptually — simplified: second hit
                self.pending_second_tap = true;
                self.pending_since = Some(now);
            }
            return None;
        }

        if self.cfg.edge_delay_ms > 0 {
            match self.pending_edge {
                Some(e) if e == edge => {
                    let since = self.pending_since.unwrap_or(now);
                    if now.duration_since(since).as_millis() as i32 >= self.cfg.edge_delay_ms {
                        self.pending_edge = None;
                        return Some(self.make_decision(neighbor, edge, x, y));
                    }
                }
                _ => {
                    self.pending_edge = Some(edge);
                    self.pending_since = Some(now);
                }
            }
            return None;
        }

        Some(self.make_decision(neighbor, edge, x, y))
    }

    fn make_decision(&self, neighbor: String, edge: Edge, x: i32, y: i32) -> EnterDecision {
        EnterDecision {
            neighbor,
            edge,
            position_pct: self.fraction_on_edge(edge, x, y),
        }
    }

    pub fn on_remote_relative(
        &mut self,
        dx: i32,
        dy: i32,
        cursor_x: &mut i32,
        cursor_y: &mut i32,
        remote: ScreenSize,
    ) -> Option<EnterDecision> {
        if self.state != SwitchState::Remote {
            return None;
        }
        *cursor_x += dx;
        *cursor_y += dy;
        if self.remote_reports {
            *cursor_x = (*cursor_x).clamp(0, remote.width.max(1) - 1);
            *cursor_y = (*cursor_y).clamp(0, remote.height.max(1) - 1);
            return None;
        }
        let past_left = *cursor_x < 0;
        let past_right = *cursor_x >= remote.width;
        let past_top = *cursor_y < 0;
        let past_bottom = *cursor_y >= remote.height;
        let diagonal = match (past_left, past_right, past_top, past_bottom) {
            (true, _, true, _) => Some(Edge::TopLeft),
            (_, true, true, _) => Some(Edge::TopRight),
            (true, _, _, true) => Some(Edge::BottomLeft),
            (_, true, _, true) => Some(Edge::BottomRight),
            _ => None,
        };
        let zx = self.diagonal_zone(remote.width);
        let zy = self.diagonal_zone(remote.height);
        let near_top = *cursor_y <= zy;
        let near_bottom = *cursor_y >= remote.height - 1 - zy;
        let near_left = *cursor_x <= zx;
        let near_right = *cursor_x >= remote.width - 1 - zx;
        let zone_diagonal = if (past_left && near_top) || (past_top && near_left) {
            Some(Edge::TopLeft)
        } else if (past_right && near_top) || (past_top && near_right) {
            Some(Edge::TopRight)
        } else if (past_left && near_bottom) || (past_bottom && near_left) {
            Some(Edge::BottomLeft)
        } else if (past_right && near_bottom) || (past_bottom && near_right) {
            Some(Edge::BottomRight)
        } else {
            None
        };
        let horizontal = if past_left {
            Some(Edge::Left)
        } else if past_right {
            Some(Edge::Right)
        } else {
            None
        };
        let vertical = if past_top {
            Some(Edge::Top)
        } else if past_bottom {
            Some(Edge::Bottom)
        } else {
            None
        };
        // Leave only through a side that leads somewhere. Other sides hold the
        // pointer, as a physical screen edge would.
        let crossed = [diagonal, zone_diagonal, horizontal, vertical]
            .into_iter()
            .flatten();
        let way_back = self
            .layout
            .edge_toward(&self.active_remote, &self.local_name)
            .is_some();
        let leave_edge = if way_back {
            crossed
                .into_iter()
                .find(|e| self.layout.neighbor(&self.active_remote, *e).is_some())
        } else {
            // No link back to this computer: any side returns, so the pointer cannot get stuck.
            [horizontal, vertical].into_iter().flatten().next()
        };
        let w = remote.width.max(1);
        let h = remote.height.max(1);
        let Some(edge) = leave_edge else {
            *cursor_x = (*cursor_x).clamp(0, w - 1);
            *cursor_y = (*cursor_y).clamp(0, h - 1);
            return None;
        };
        let pct = match edge {
            Edge::Left | Edge::Right => (*cursor_y).clamp(0, h - 1) as f32 / h as f32,
            Edge::Top | Edge::Bottom => (*cursor_x).clamp(0, w - 1) as f32 / w as f32,
            _ => 0.5,
        };
        Some(EnterDecision {
            neighbor: self.local_name.clone(),
            edge,
            position_pct: pct,
        })
    }
}

/// Of the sides the pointer touches, the one it is pushed through hardest,
/// among those `ok` accepts. At a corner pixel the old order always chose the
/// left/right side, so pushing up or down there never switched.
fn pick_edge(touched: &[Edge], raw: (i32, i32), ok: impl Fn(Edge) -> bool) -> Option<Edge> {
    let mut best: Option<(Edge, i32)> = None;
    for &e in touched.iter().filter(|e| ok(**e)) {
        let push = outward(e, raw.0, raw.1);
        if best.is_none_or(|(_, p)| push > p) {
            best = Some((e, push));
        }
    }
    best.map(|(e, _)| e)
}

fn desk_bounds(list: &[Rect]) -> (i32, i32, i32, i32) {
    (
        list.iter().map(|r| r.left).min().unwrap_or(0),
        list.iter().map(|r| r.right).max().unwrap_or(0),
        list.iter().map(|r| r.top).min().unwrap_or(0),
        list.iter().map(|r| r.bottom).max().unwrap_or(0),
    )
}

fn is_corner(e: Edge) -> bool {
    matches!(
        e,
        Edge::TopLeft | Edge::TopRight | Edge::BottomLeft | Edge::BottomRight
    )
}

#[cfg(test)]
mod tests {

    #[test]
    fn corner_pixel_pushed_up_switches_up() {
        let mut layout_map = csc_config::LayoutMap::new();
        let mut e = BTreeMap::new();
        e.insert("top".into(), "REMOTE".into());
        layout_map.insert("LOCAL".into(), e);
        let mut sw = SwitchController::default();
        let cfg = SwitchConfig {
            diagonal_zone_px: 1,
            corner_dead_zone_px: 0,
            ..SwitchConfig::default()
        };
        sw.configure(
            "LOCAL".into(),
            ScreenSize {
                width: 100,
                height: 100,
            },
            cfg,
            ScreenLayout::load(&layout_map),
        );
        let t = Instant::now();
        // Still inside. The top-left pixel is the other computer.
        assert!(sw.on_local_cursor(2, 2, Some((0, -4)), t).is_none());
        let got = sw
            .on_local_cursor(0, 0, Some((0, -4)), t)
            .expect("switches up");
        assert_eq!(got.edge, Edge::Top);
    }

    #[test]
    fn pick_edge_follows_the_push() {
        let both = [Edge::Left, Edge::Top];
        assert_eq!(pick_edge(&both, (0, -5), |_| true), Some(Edge::Top));
        assert_eq!(pick_edge(&both, (-5, 0), |_| true), Some(Edge::Left));
        assert_eq!(
            pick_edge(&both, (-5, 0), |e| e == Edge::Top),
            Some(Edge::Top)
        );
        assert_eq!(pick_edge(&[], (0, 0), |_| true), None);
    }

    use super::*;
    use std::collections::BTreeMap;

    #[test]
    fn edge_switches_on_the_outer_pixel() {
        let mut layout_map = csc_config::LayoutMap::new();
        let mut e = BTreeMap::new();
        e.insert("right".into(), "REMOTE".into());
        layout_map.insert("LOCAL".into(), e);
        let mut sw = SwitchController::default();
        sw.configure(
            "LOCAL".into(),
            ScreenSize {
                width: 100,
                height: 100,
            },
            SwitchConfig::default(),
            ScreenLayout::load(&layout_map),
        );
        let t = Instant::now();
        assert!(sw.on_local_cursor(95, 50, Some((0, 0)), t).is_none());
        let d = sw
            .on_local_cursor(99, 50, Some((4, 0)), t)
            .expect("outer pixel");
        assert_eq!(d.edge, Edge::Right);
    }

    #[test]
    fn fast_arrival_waits_until_the_pointer_is_drawn_on_the_edge() {
        let mut layout_map = csc_config::LayoutMap::new();
        let mut e = BTreeMap::new();
        e.insert("top".into(), "REMOTE".into());
        layout_map.insert("LOCAL".into(), e);
        let mut sw = SwitchController::default();
        sw.configure(
            "LOCAL".into(),
            ScreenSize {
                width: 100,
                height: 100,
            },
            SwitchConfig::default(),
            ScreenLayout::load(&layout_map),
        );
        let t = Instant::now();
        // One sample carries the cursor from mid-screen onto the top edge.
        assert!(sw.on_local_cursor(50, 40, Some((0, -10)), t).is_none());
        let d = sw
            .on_local_cursor(50, 0, Some((0, -80)), t)
            .expect("outer pixel");
        assert_eq!(d.edge, Edge::Top);
    }

    /// Move onto `(x, y)` along `out`. The sample that lands there switches.
    fn touch_edge(sw: &mut SwitchController, x: i32, y: i32, out: (i32, i32)) -> EnterDecision {
        let t = Instant::now();
        assert!(
            sw.on_local_cursor(x - out.0, y - out.1, Some(out), t)
                .is_none(),
            "one pixel inside stays on this screen"
        );
        sw.on_local_cursor(x, y, Some(out), t)
            .expect("outer pixel switches")
    }

    #[test]
    fn silent_raw_motion_switches_on_contact() {
        let mut layout_map = csc_config::LayoutMap::new();
        let mut e = BTreeMap::new();
        e.insert("right".into(), "REMOTE".into());
        layout_map.insert("LOCAL".into(), e);
        let mut sw = SwitchController::default();
        sw.configure(
            "LOCAL".into(),
            ScreenSize {
                width: 100,
                height: 100,
            },
            SwitchConfig::default(),
            ScreenLayout::load(&layout_map),
        );
        let t = Instant::now();
        // A touchpad whose raw input never arrives still switches on the outer pixel.
        for x in [80, 85, 90, 95] {
            assert!(sw.on_local_cursor(x, 50, Some((0, 0)), t).is_none());
        }
        assert!(sw.on_local_cursor(99, 50, Some((0, 0)), t).is_some());
    }

    #[test]
    fn controlled_side_reports_only_real_outer_edges() {
        let mut sw = SwitchController::default();
        let screen = ScreenSize {
            width: 3360,
            height: 1080,
        };
        sw.configure(
            "B".into(),
            screen,
            SwitchConfig::default(),
            ScreenLayout::default(),
        );
        // Two monitors, the right one shorter: its bottom is not the desktop bottom,
        // but the host's bounding box would put the edge at 1080.
        sw.set_screen(
            screen,
            vec![
                Rect {
                    left: 0,
                    top: 0,
                    right: 1920,
                    bottom: 1080,
                },
                Rect {
                    left: 1920,
                    top: 0,
                    right: 3360,
                    bottom: 900,
                },
            ],
        );
        let mut push = EdgePush;
        assert!(
            sw.probe_edge(&mut push, 1919, 500, (50, 0), (0, 0))
                .is_none(),
            "inner border"
        );
        // Sitting on the shared pixel, or sliding along it, does not leave.
        assert!(sw
            .probe_edge(&mut push, 3359, 450, (0, 8), (0, 8))
            .is_none());
        assert!(sw
            .probe_edge(&mut push, 3359, 450, (-3, 0), (0, 0))
            .is_none());
        let r = sw
            .probe_edge(&mut push, 3359, 450, (4, 0), (0, 0))
            .expect("pushed outward off the shared pixel");
        assert_eq!((r.edge, r.corner), (Edge::Right, None));
        assert!((r.position_pct - 0.5).abs() < 0.01);
    }

    #[test]
    fn host_follows_edge_reports_from_the_remote() {
        let mut sw = SwitchController::default();
        sw.configure(
            "LOCAL".into(),
            ScreenSize {
                width: 100,
                height: 100,
            },
            SwitchConfig::default(),
            two_way_layout(),
        );
        let remote = ScreenSize {
            width: 200,
            height: 100,
        };
        let (mut x, mut y) = (0, 0);
        sw.confirm_entered(Edge::Left, 0.5, remote, &mut x, &mut y, "REMOTE");
        sw.set_remote_reports(true);
        // The estimate alone never leaves.
        assert!(sw
            .on_remote_relative(-400, 0, &mut x, &mut y, remote)
            .is_none());
        let report = |edge| EdgeReport {
            edge,
            corner: None,
            position_pct: 0.3,
        };
        assert!(sw.on_remote_edge(&report(Edge::Right)).is_none());
        let back = sw
            .on_remote_edge(&report(Edge::Left))
            .expect("left leads home");
        assert_eq!(back.edge, Edge::Left);
        assert!((back.position_pct - 0.3).abs() < 1e-6);
    }

    #[test]
    fn edge_enter_right() {
        let mut layout_map = csc_config::LayoutMap::new();
        let mut e = BTreeMap::new();
        e.insert("right".into(), "REMOTE".into());
        layout_map.insert("LOCAL".into(), e);
        let mut sw = SwitchController::default();
        sw.configure(
            "LOCAL".into(),
            ScreenSize {
                width: 100,
                height: 100,
            },
            SwitchConfig::default(),
            ScreenLayout::load(&layout_map),
        );
        let d = touch_edge(&mut sw, 99, 50, (1, 0));
        assert_eq!(d.neighbor, "REMOTE");
        assert_eq!(d.edge, Edge::Right);
    }

    #[test]
    fn corner_enters_diagonal_neighbor() {
        let mut layout_map = csc_config::LayoutMap::new();
        let mut e = BTreeMap::new();
        e.insert("top_right".into(), "CORNER".into());
        e.insert("right".into(), "SIDE".into());
        layout_map.insert("LOCAL".into(), e);
        let mut sw = SwitchController::default();
        sw.configure(
            "LOCAL".into(),
            ScreenSize {
                width: 100,
                height: 80,
            },
            SwitchConfig::default(),
            ScreenLayout::load(&layout_map),
        );
        let d = touch_edge(&mut sw, 99, 0, (1, -1));
        assert_eq!(d.neighbor, "CORNER");
        assert_eq!(d.edge, Edge::TopRight);
    }

    fn two_way_layout() -> ScreenLayout {
        let mut layout_map = csc_config::LayoutMap::new();
        let mut host = BTreeMap::new();
        host.insert("right".into(), "REMOTE".into());
        layout_map.insert("LOCAL".into(), host);
        let mut remote = BTreeMap::new();
        remote.insert("left".into(), "LOCAL".into());
        layout_map.insert("REMOTE".into(), remote);
        ScreenLayout::load(&layout_map)
    }

    #[test]
    fn edge_between_monitors_is_not_an_exit() {
        // 1920x1080 left monitor, shorter 1440x900 right monitor.
        let mut sw = SwitchController::default();
        sw.configure(
            "LOCAL".into(),
            ScreenSize {
                width: 3360,
                height: 1080,
            },
            SwitchConfig::default(),
            two_way_layout(),
        );
        sw.set_screen(
            ScreenSize {
                width: 3360,
                height: 1080,
            },
            vec![
                Rect {
                    left: 0,
                    top: 0,
                    right: 1920,
                    bottom: 1080,
                },
                Rect {
                    left: 1920,
                    top: 0,
                    right: 3360,
                    bottom: 900,
                },
            ],
        );
        // Inner border between the monitors does not switch, even below the short monitor.
        assert!(sw
            .on_local_cursor(1919, 500, None, Instant::now())
            .is_none());
        assert!(sw
            .on_local_cursor(1919, 1000, None, Instant::now())
            .is_none());
        // Right side of the right monitor switches, with the position on that monitor.
        let d = touch_edge(&mut sw, 3359, 450, (1, 0));
        assert_eq!(d.edge, Edge::Right);
        assert!((d.position_pct - 0.5).abs() < 0.01);
    }

    #[test]
    fn remote_side_without_neighbor_holds_the_pointer() {
        let mut sw = SwitchController::default();
        sw.configure(
            "LOCAL".into(),
            ScreenSize {
                width: 100,
                height: 100,
            },
            SwitchConfig::default(),
            two_way_layout(),
        );
        let remote = ScreenSize {
            width: 200,
            height: 100,
        };
        let (mut x, mut y) = (0, 0);
        sw.confirm_entered(Edge::Left, 0.5, remote, &mut x, &mut y, "REMOTE");
        // Far right side has nothing: stay on the remote, clamped.
        assert!(sw
            .on_remote_relative(500, 0, &mut x, &mut y, remote)
            .is_none());
        assert_eq!(x, 199);
        // Left side leads back to LOCAL.
        let back = sw
            .on_remote_relative(-400, 0, &mut x, &mut y, remote)
            .expect("return");
        assert_eq!(back.edge, Edge::Left);
    }

    #[test]
    fn corner_without_diagonal_uses_the_side() {
        let mut layout_map = csc_config::LayoutMap::new();
        let mut e = BTreeMap::new();
        e.insert("right".into(), "SIDE".into());
        layout_map.insert("LOCAL".into(), e);
        let mut sw = SwitchController::default();
        sw.configure(
            "LOCAL".into(),
            ScreenSize {
                width: 100,
                height: 80,
            },
            SwitchConfig::default(),
            ScreenLayout::load(&layout_map),
        );
        let side = touch_edge(&mut sw, 99, 0, (1, 0));
        assert_eq!(side.neighbor, "SIDE");
        assert_eq!(side.edge, Edge::Right);
    }

    #[test]
    fn diagonal_zone_reaches_corner_of_uneven_monitors() {
        let mut layout_map = csc_config::LayoutMap::new();
        let mut host = BTreeMap::new();
        host.insert("bottom_right".into(), "REMOTE".into());
        layout_map.insert("LOCAL".into(), host);
        let mut remote = BTreeMap::new();
        remote.insert("top_left".into(), "LOCAL".into());
        layout_map.insert("REMOTE".into(), remote);
        let mut sw = SwitchController::default();
        sw.configure(
            "LOCAL".into(),
            ScreenSize {
                width: 3360,
                height: 1080,
            },
            SwitchConfig::default(),
            ScreenLayout::load(&layout_map),
        );
        sw.set_screen(
            ScreenSize {
                width: 3360,
                height: 1080,
            },
            vec![
                Rect {
                    left: 0,
                    top: 0,
                    right: 1920,
                    bottom: 1080,
                },
                Rect {
                    left: 1920,
                    top: 0,
                    right: 3360,
                    bottom: 900,
                },
            ],
        );
        // Right side of the shorter monitor, near its bottom.
        let d = touch_edge(&mut sw, 3359, 860, (1, 1));
        assert_eq!(d.edge, Edge::BottomRight);
        // Bottom side of the taller monitor, near its right end is inner: no switch.
        assert!(sw
            .on_local_cursor(1900, 1079, None, Instant::now())
            .is_none());
        // Middle of the right side does not switch.
        assert!(sw
            .on_local_cursor(3359, 450, None, Instant::now())
            .is_none());

        let remote_size = ScreenSize {
            width: 1000,
            height: 800,
        };
        let (mut x, mut y) = (0, 0);
        sw.confirm_entered(
            d.edge,
            d.position_pct,
            remote_size,
            &mut x,
            &mut y,
            "REMOTE",
        );
        // Leaving across the top near the left end returns diagonally.
        x = 40;
        y = 5;
        let back = sw
            .on_remote_relative(0, -20, &mut x, &mut y, remote_size)
            .expect("return");
        assert_eq!(back.edge, Edge::TopLeft);
    }

    #[test]
    fn taskbar_keeps_the_pointer_on_this_computer() {
        let mut layout_map = csc_config::LayoutMap::new();
        let mut host = BTreeMap::new();
        host.insert("bottom".into(), "BELOW".into());
        host.insert("right".into(), "SIDE".into());
        host.insert("bottom_right".into(), "CORNER".into());
        layout_map.insert("LOCAL".into(), host);
        let mut sw = SwitchController::default();
        sw.configure(
            "LOCAL".into(),
            ScreenSize {
                width: 1920,
                height: 1080,
            },
            SwitchConfig::default(),
            ScreenLayout::load(&layout_map),
        );
        sw.set_screen(
            ScreenSize {
                width: 1920,
                height: 1080,
            },
            vec![Rect {
                left: 0,
                top: 0,
                right: 1920,
                bottom: 1080,
            }],
        );
        // 40px taskbar along the bottom. The show-desktop corner is inside it.
        sw.set_work_areas(vec![Rect {
            left: 0,
            top: 0,
            right: 1920,
            bottom: 1040,
        }]);
        let now = Instant::now();
        // Icons in the taskbar are not the outer edge.
        assert!(sw.on_local_cursor(1910, 1070, Some((8, 20)), now).is_none());
        assert!(sw.on_local_cursor(1910, 1070, Some((8, 20)), now).is_none());
        // The outer corner pixel still reaches the computer placed there.
        let corner = touch_edge(&mut sw, 1919, 1079, (1, 1));
        assert_eq!(corner.neighbor, "CORNER");
        assert_eq!(corner.edge, Edge::BottomRight);
        // The right edge above the taskbar still leaves this computer.
        let side = touch_edge(&mut sw, 1919, 500, (1, 0));
        assert_eq!(side.neighbor, "SIDE");
        assert_eq!(side.edge, Edge::Right);
        // The bottom pixel under the taskbar still reaches the computer below.
        let below = touch_edge(&mut sw, 960, 1079, (0, 1));
        assert_eq!(below.neighbor, "BELOW");
        assert_eq!(below.edge, Edge::Bottom);
    }

    #[test]
    fn bottom_pixel_and_other_screen_last_pixel_are_one() {
        let mut layout_map = csc_config::LayoutMap::new();
        let mut upper = BTreeMap::new();
        upper.insert("bottom".into(), "LOWER".into());
        layout_map.insert("UPPER".into(), upper);
        let mut lower = BTreeMap::new();
        lower.insert("top".into(), "UPPER".into());
        layout_map.insert("LOWER".into(), lower);
        let layout = ScreenLayout::load(&layout_map);
        let upper_size = ScreenSize {
            width: 1920,
            height: 1024,
        };
        let lower_size = ScreenSize {
            width: 1920,
            height: 800,
        };
        let mut sw = SwitchController::default();
        sw.configure("LOWER".into(), lower_size, SwitchConfig::default(), layout);
        let (mut x, mut y) = (0, 0);
        // Entering the upper screen from below lands on its bottom pixel, 0x3FF.
        sw.confirm_entered(Edge::Bottom, 0.25, upper_size, &mut x, &mut y, "UPPER");
        assert_eq!((x, y), (480, 0x3FF));
        assert!(sw.cursor_on_shared_pixel(x, y, upper_size));
        assert!(!sw.cursor_on_shared_pixel(x, 0x3FF - 1, upper_size));
        // The lower screen's top pixel is that same seam.
        sw.confirm_entered(Edge::Top, 0.25, lower_size, &mut x, &mut y, "LOWER");
        assert_eq!(y, 0);
        assert!(sw.cursor_on_shared_pixel(x, 0, lower_size));
        assert!(!sw.cursor_on_shared_pixel(x, 1, lower_size));
    }

    #[test]
    fn different_heights_keep_the_fraction_of_the_real_edge() {
        let mut sw = SwitchController::default();
        let wide = ScreenSize {
            width: 4480,
            height: 1440,
        };
        sw.configure(
            "LOCAL".into(),
            wide,
            SwitchConfig::default(),
            ScreenLayout::default(),
        );
        // 1080p on the left, vertically centered beside a 1440p panel.
        // The bounding box is 1440 tall, so 25% of the box is y=360.
        sw.set_screen(
            wide,
            vec![
                Rect {
                    left: 0,
                    top: 180,
                    right: 1920,
                    bottom: 1260,
                },
                Rect {
                    left: 1920,
                    top: 0,
                    right: 4480,
                    bottom: 1440,
                },
            ],
        );
        let (mut x, mut y) = (0, 0);
        sw.confirm_entered(Edge::Left, 0.25, wide, &mut x, &mut y, "");
        assert_eq!((x, y), (0, 180 + 270));
        // Leaving that pixel reports the same fraction, and lands back on it.
        let pct = sw.fraction_on_edge(Edge::Left, x, y);
        assert!((pct - 0.25).abs() < 0.001);
        assert_eq!(sw.place_on_edge(Edge::Left, pct), (x, y));

        // Top-aligned: the left edge is only the shorter monitor. Its middle
        // is y=540, not the middle of the 1440 box (y=720).
        sw.set_screen(
            wide,
            vec![
                Rect {
                    left: 0,
                    top: 0,
                    right: 1920,
                    bottom: 1080,
                },
                Rect {
                    left: 1920,
                    top: 0,
                    right: 4480,
                    bottom: 1440,
                },
            ],
        );
        sw.confirm_entered(Edge::Left, 0.5, wide, &mut x, &mut y, "");
        assert_eq!((x, y), (0, 540));
        // The bottom edge belongs to the taller monitor only.
        sw.confirm_entered(Edge::Bottom, 0.0, wide, &mut x, &mut y, "");
        assert_eq!((x, y), (1920, 1439));

        // Two left monitors with a gap. Halfway skips the gap.
        let gapped = ScreenSize {
            width: 100,
            height: 250,
        };
        sw.set_screen(
            gapped,
            vec![
                Rect {
                    left: 0,
                    top: 0,
                    right: 100,
                    bottom: 100,
                },
                Rect {
                    left: 0,
                    top: 150,
                    right: 100,
                    bottom: 250,
                },
            ],
        );
        sw.confirm_entered(Edge::Left, 0.5, gapped, &mut x, &mut y, "");
        assert_eq!((x, y), (0, 150));
    }
}
