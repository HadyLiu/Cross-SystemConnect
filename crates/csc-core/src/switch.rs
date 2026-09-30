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
}

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

    /// Corner that the pointer is in, using the configured dead-zone size.
    fn diagonal_at(&self, x: i32, y: i32) -> Option<Edge> {
        let d = self.cfg.corner_dead_zone_px.max(0);
        let m = self.monitor_at(x, y);
        let (ol, or, ot, ob) = self.outer_sides(&m);
        let left = ol && x <= m.left + d;
        let right = or && x >= m.right - 1 - d;
        let top = ot && y <= m.top + d;
        let bottom = ob && y >= m.bottom - 1 - d;
        if left && top {
            Some(Edge::TopLeft)
        } else if right && top {
            Some(Edge::TopRight)
        } else if left && bottom {
            Some(Edge::BottomLeft)
        } else if right && bottom {
            Some(Edge::BottomRight)
        } else {
            None
        }
    }

    pub fn on_local_cursor(&mut self, x: i32, y: i32, now: Instant) -> Option<EnterDecision> {
        if !self.enabled || self.state != SwitchState::Local {
            return None;
        }
        let edge = if let Some(diag) = self.diagonal_at(x, y) {
            if self.layout.neighbor(&self.local_name, diag).is_some() {
                diag
            } else {
                // No machine on this corner: keep the corner from triggering a side.
                self.pending_edge = None;
                return None;
            }
        } else if let Some(edge) = self.hit_outer_edge(x, y) {
            edge
        } else {
            self.pending_edge = None;
            self.pending_second_tap = false;
            return None;
        };
        let neighbor = self.layout.neighbor(&self.local_name, edge)?.to_string();

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
        let crossed = [diagonal, horizontal, vertical].into_iter().flatten();
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

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
        let d = sw.on_local_cursor(99, 50, Instant::now()).expect("enter");
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
        let d = sw.on_local_cursor(99, 0, Instant::now()).expect("diagonal");
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
            ScreenSize { width: 3360, height: 1080 },
            SwitchConfig::default(),
            two_way_layout(),
        );
        sw.set_screen(
            ScreenSize { width: 3360, height: 1080 },
            vec![
                Rect { left: 0, top: 0, right: 1920, bottom: 1080 },
                Rect { left: 1920, top: 0, right: 3360, bottom: 900 },
            ],
        );
        // Inner border between the monitors does not switch, even below the short monitor.
        assert!(sw.on_local_cursor(1919, 500, Instant::now()).is_none());
        assert!(sw.on_local_cursor(1919, 1000, Instant::now()).is_none());
        // Right side of the right monitor switches, with the position on that monitor.
        let d = sw.on_local_cursor(3359, 450, Instant::now()).expect("right edge");
        assert_eq!(d.edge, Edge::Right);
        assert!((d.position_pct - 0.5).abs() < 0.01);
    }

    #[test]
    fn remote_side_without_neighbor_holds_the_pointer() {
        let mut sw = SwitchController::default();
        sw.configure(
            "LOCAL".into(),
            ScreenSize { width: 100, height: 100 },
            SwitchConfig::default(),
            two_way_layout(),
        );
        let remote = ScreenSize { width: 200, height: 100 };
        let (mut x, mut y) = (0, 0);
        sw.confirm_entered(Edge::Left, 0.5, remote, &mut x, &mut y, "REMOTE");
        // Far right side has nothing: stay on the remote, clamped.
        assert!(sw.on_remote_relative(500, 0, &mut x, &mut y, remote).is_none());
        assert_eq!(x, 199);
        // Left side leads back to LOCAL.
        let back = sw
            .on_remote_relative(-400, 0, &mut x, &mut y, remote)
            .expect("return");
        assert_eq!(back.edge, Edge::Left);
    }

    #[test]
    fn corner_without_diagonal_does_not_switch() {
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
        assert!(sw.on_local_cursor(99, 0, Instant::now()).is_none());
        let side = sw
            .on_local_cursor(99, 40, Instant::now())
            .expect("right edge");
        assert_eq!(side.neighbor, "SIDE");
        assert_eq!(side.edge, Edge::Right);
    }
}
