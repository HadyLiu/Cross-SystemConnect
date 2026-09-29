#![cfg_attr(not(windows), allow(dead_code))]

use crate::layout::ScreenLayout;
use csc_config::SwitchConfig;
use csc_platform::ScreenSize;
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
        }
    }

    pub fn confirm_returned(&mut self) {
        self.unlock_to_local();
    }

    fn in_corner_dead_zone(&self, x: i32, y: i32) -> bool {
        let d = self.cfg.corner_dead_zone_px;
        let w = self.screen.width;
        let h = self.screen.height;
        (x <= d || x >= w - d) && (y <= d || y >= h - d)
    }

    fn hit_outer_edge(&self, x: i32, y: i32) -> Option<Edge> {
        let w = self.screen.width;
        let h = self.screen.height;
        if x <= 0 {
            Some(Edge::Left)
        } else if x >= w - 1 {
            Some(Edge::Right)
        } else if y <= 0 {
            Some(Edge::Top)
        } else if y >= h - 1 {
            Some(Edge::Bottom)
        } else {
            None
        }
    }

    pub fn on_local_cursor(&mut self, x: i32, y: i32, now: Instant) -> Option<EnterDecision> {
        if !self.enabled || self.state != SwitchState::Local {
            return None;
        }
        if self.in_corner_dead_zone(x, y) {
            self.pending_edge = None;
            return None;
        }
        let Some(edge) = self.hit_outer_edge(x, y) else {
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
        let pct = match edge {
            Edge::Left | Edge::Right => {
                if self.screen.height > 0 {
                    y as f32 / self.screen.height as f32
                } else {
                    0.5
                }
            }
            Edge::Top | Edge::Bottom => {
                if self.screen.width > 0 {
                    x as f32 / self.screen.width as f32
                } else {
                    0.5
                }
            }
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
        let leave_edge = if *cursor_x < 0 {
            Some(Edge::Left)
        } else if *cursor_x >= remote.width {
            Some(Edge::Right)
        } else if *cursor_y < 0 {
            Some(Edge::Top)
        } else if *cursor_y >= remote.height {
            Some(Edge::Bottom)
        } else {
            None
        };
        leave_edge.map(|edge| EnterDecision {
            neighbor: self.local_name.clone(),
            edge,
            position_pct: 0.5,
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
}
