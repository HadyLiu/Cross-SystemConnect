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

/// How far the pointer pushes outward after it reached an edge.
/// The system holds the cursor at the edge, so the push is measured as the
/// part of the raw hand movement that did not move the cursor.
#[derive(Debug, Default, Clone)]
pub struct EdgePush {
    edge: Option<Edge>,
    acc: i32,
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
    /// Add one step at `edge`: `raw` is the hand movement, `moved` how far the
    /// cursor actually went. True once the push reaches `need` pixels.
    pub fn step(&mut self, edge: Edge, raw: (i32, i32), moved: (i32, i32), need: i32) -> bool {
        if self.edge != Some(edge) {
            self.edge = Some(edge);
            self.acc = 0;
        }
        let push = outward(edge, raw.0, raw.1);
        if push < 0 {
            self.acc = 0;
        } else {
            self.acc += (push - outward(edge, moved.0, moved.1).max(0)).max(0);
        }
        self.acc >= need.max(0)
    }

    pub fn reset(&mut self) {
        self.edge = None;
        self.acc = 0;
    }
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
    cfg: SwitchConfig,
    layout: ScreenLayout,
    state: SwitchState,
    /// Host currently receiving the pointer, when `state` is remote.
    active_remote: String,
    enabled: bool,
    pending_edge: Option<Edge>,
    pending_since: Option<Instant>,
    pending_second_tap: bool,
    push: EdgePush,
    last_local: Option<(i32, i32)>,
    /// Consecutive steps where the cursor moved but raw motion stayed silent.
    raw_misses: u8,
    /// The active remote reports its own edge contact (EdgeHit).
    remote_reports: bool,
}

/// Raw motion that missed this many cursor movements in a row is not used:
/// touchpads, tablets and remote sessions may deliver none, and the edge
/// must then switch on contact instead of waiting for a push it cannot see.
const RAW_MISS_LIMIT: u8 = 3;

impl Default for SwitchController {
    fn default() -> Self {
        Self {
            local_name: String::new(),
            screen: ScreenSize::default(),
            monitors: Vec::new(),
            cfg: SwitchConfig::default(),
            layout: ScreenLayout::default(),
            state: SwitchState::Local,
            active_remote: String::new(),
            enabled: true,
            pending_edge: None,
            pending_since: None,
            pending_second_tap: false,
            push: EdgePush::default(),
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

    pub fn edge_push_px(&self) -> i32 {
        self.cfg.edge_push_px
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
    /// `moved`, while the host sent `raw`. Reports an outer edge once the
    /// pointer pushed `edge_push_px` past it. Any outer edge counts; the
    /// keyboard host knows which ones lead somewhere.
    pub fn probe_edge(
        &self,
        push: &mut EdgePush,
        x: i32,
        y: i32,
        raw: (i32, i32),
        moved: (i32, i32),
    ) -> Option<EdgeReport> {
        let Some(edge) = self.hit_outer_edge(x, y) else {
            push.reset();
            return None;
        };
        let corner = self.diagonal_at(x, y);
        let along = corner.unwrap_or(edge);
        if !push.step(along, raw, moved, self.cfg.edge_push_px) {
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

    /// Neighbor of the active remote screen on `edge`, if the layout has one.
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

    pub fn confirm_returned(&mut self) {
        self.unlock_to_local();
    }

    fn hit_outer_edge(&self, x: i32, y: i32) -> Option<Edge> {
        let m = self.monitor_at(x, y);
        let (ol, or, ot, ob) = self.outer_sides(&m);
        if ol && x <= m.left {
            Some(Edge::Left)
        } else if or && x >= m.right - 1 {
            Some(Edge::Right)
        } else if ot && y <= m.top {
            Some(Edge::Top)
        } else if ob && y >= m.bottom - 1 {
            Some(Edge::Bottom)
        } else {
            None
        }
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
    /// when the platform can measure it; without it the edge switches on contact.
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
                    tracing::info!(
                        "raw mouse motion available; edges need a push of {} px",
                        self.cfg.edge_push_px
                    );
                } else {
                    tracing::info!(
                        "raw mouse motion silent while the cursor moves; edges switch on contact"
                    );
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
        if !self.enabled || self.state != SwitchState::Local {
            return None;
        }
        let moved = self
            .last_local
            .map(|(lx, ly)| (x - lx, y - ly))
            .unwrap_or((0, 0));
        self.last_local = Some((x, y));
        let raw = self.usable_raw(raw, moved);
        let diagonal = self
            .diagonal_at(x, y)
            .filter(|d| self.layout.neighbor(&self.local_name, *d).is_some());
        let edge = if let Some(diag) = diagonal {
            diag
        } else if let Some(edge) = self.hit_outer_edge(x, y) {
            edge
        } else {
            self.pending_edge = None;
            self.pending_second_tap = false;
            self.push.reset();
            return None;
        };
        let neighbor = self.layout.neighbor(&self.local_name, edge)?.to_string();
        if let Some(raw) = raw {
            if !self.push.step(edge, raw, moved, self.cfg.edge_push_px) {
                return None;
            }
        }

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
        let m = self.monitor_at(x, y);
        let pct = match edge {
            Edge::Left | Edge::Right => {
                if m.height() > 0 {
                    (y - m.top) as f32 / m.height() as f32
                } else {
                    0.5
                }
            }
            Edge::Top | Edge::Bottom => {
                if m.width() > 0 {
                    (x - m.left) as f32 / m.width() as f32
                } else {
                    0.5
                }
            }
            Edge::TopLeft | Edge::TopRight | Edge::BottomLeft | Edge::BottomRight => 0.5,
        };
        EnterDecision {
            neighbor,
            edge,
            position_pct: pct.clamp(0.0, 1.0),
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

fn is_corner(e: Edge) -> bool {
    matches!(
        e,
        Edge::TopLeft | Edge::TopRight | Edge::BottomLeft | Edge::BottomRight
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    #[test]
    fn edge_needs_a_push_past_it() {
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
        // Arriving at the edge: the cursor moved as far as the hand did.
        assert!(sw.on_local_cursor(95, 50, Some((0, 0)), t).is_none());
        assert!(sw.on_local_cursor(99, 50, Some((4, 0)), t).is_none());
        // Sliding along the edge does not count.
        assert!(sw.on_local_cursor(99, 60, Some((0, 10)), t).is_none());
        assert!(sw.on_local_cursor(99, 60, Some((5, 0)), t).is_none());
        // Pulling back resets the push.
        assert!(sw.on_local_cursor(99, 60, Some((-1, 0)), t).is_none());
        assert!(sw.on_local_cursor(99, 60, Some((5, 0)), t).is_none());
        let d = sw
            .on_local_cursor(99, 60, Some((3, 0)), t)
            .expect("pushed 8 px");
        assert_eq!(d.edge, Edge::Right);
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
        // A touchpad whose raw input never arrives: the cursor moves, raw stays zero.
        for x in [80, 85, 90, 95] {
            assert!(sw.on_local_cursor(x, 50, Some((0, 0)), t).is_none());
        }
        let d = sw
            .on_local_cursor(99, 50, Some((0, 0)), t)
            .expect("contact is enough");
        assert_eq!(d.edge, Edge::Right);

        // Once raw motion follows the hand again, the push is required.
        sw.unlock_to_local();
        assert!(sw.on_local_cursor(90, 50, Some((0, 0)), t).is_none());
        assert!(sw.on_local_cursor(95, 50, Some((5, 0)), t).is_none());
        assert!(sw.on_local_cursor(99, 50, Some((4, 0)), t).is_none());
        assert!(sw.on_local_cursor(99, 50, Some((8, 0)), t).is_some());
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
        let mut push = EdgePush::default();
        assert!(
            sw.probe_edge(&mut push, 1919, 500, (50, 0), (0, 0))
                .is_none(),
            "inner border"
        );
        assert!(sw
            .probe_edge(&mut push, 3359, 450, (4, 0), (4, 0))
            .is_none());
        let r = sw
            .probe_edge(&mut push, 3359, 450, (9, 0), (0, 0))
            .expect("pushed through the right side");
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
        let d = sw
            .on_local_cursor(99, 50, None, Instant::now())
            .expect("enter");
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
        let d = sw
            .on_local_cursor(99, 0, None, Instant::now())
            .expect("diagonal");
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
        let d = sw
            .on_local_cursor(3359, 450, None, Instant::now())
            .expect("right edge");
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
        let side = sw
            .on_local_cursor(99, 0, None, Instant::now())
            .expect("right edge");
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
        let d = sw
            .on_local_cursor(3359, 860, None, Instant::now())
            .expect("diagonal");
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
}
