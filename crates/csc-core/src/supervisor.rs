//! Decides when the network must be rebuilt without the user pressing
//! refresh: the selected adapter address came back, the adapters changed, or
//! a controlled computer has been without its keyboard host for a while.

use std::time::{Duration, Instant};

const FIRST_RETRY: Duration = Duration::from_millis(500);
const RETRY_STEP: Duration = Duration::from_millis(500);
const MAX_RETRY: Duration = Duration::from_secs(5);

#[derive(Debug)]
pub struct Supervisor {
    search: String,
    /// Whether the selected address was on an adapter at the last check.
    present: Option<bool>,
    addresses: Vec<String>,
    ever_linked: bool,
    down_since: Option<Instant>,
    retry: Duration,
}

impl Default for Supervisor {
    fn default() -> Self {
        Self {
            search: String::new(),
            present: None,
            addresses: Vec::new(),
            ever_linked: false,
            down_since: None,
            retry: FIRST_RETRY,
        }
    }
}

impl Supervisor {
    /// Latest adapter addresses. Returns why the network should restart, if it should.
    pub fn on_adapters(&mut self, search: &str, addresses: &[String]) -> Option<String> {
        let mut now_list: Vec<String> = addresses.to_vec();
        now_list.sort();
        if search != self.search {
            // A new selection restarts the network through the settings path already.
            self.search = search.to_string();
            self.present = None;
            self.addresses = now_list;
            return None;
        }
        let why = if !search.is_empty() {
            let present = now_list.iter().any(|a| a == search);
            let back = self.present == Some(false) && present;
            self.present = Some(present);
            back.then(|| format!("{search} is available again"))
        } else {
            let changed =
                !self.addresses.is_empty() && !now_list.is_empty() && now_list != self.addresses;
            changed.then(|| "network adapters changed".to_string())
        };
        if !now_list.is_empty() {
            self.addresses = now_list;
        }
        why
    }

    /// Link state of a controlled computer. Returns a reason to rebuild when
    /// it has been without its keyboard host for long enough.
    pub fn on_link(&mut self, connected: bool, now: Instant) -> Option<String> {
        if connected {
            self.ever_linked = true;
            self.down_since = None;
            self.retry = FIRST_RETRY;
            return None;
        }
        if !self.ever_linked {
            return None;
        }
        let since = *self.down_since.get_or_insert(now);
        if now.duration_since(since) < self.retry {
            return None;
        }
        let waited = self.retry;
        self.down_since = Some(now);
        self.retry = (self.retry + RETRY_STEP).min(MAX_RETRY);
        Some(format!("no keyboard host for {}s", waited.as_secs()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn list(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn selected_address_coming_back_restarts() {
        let mut s = Supervisor::default();
        assert!(s.on_adapters("10.0.0.2", &list(&["10.0.0.2"])).is_none());
        assert!(s.on_adapters("10.0.0.2", &list(&["10.0.0.2"])).is_none());
        assert!(s.on_adapters("10.0.0.2", &list(&["192.168.1.5"])).is_none());
        assert!(s
            .on_adapters("10.0.0.2", &list(&["192.168.1.5", "10.0.0.2"]))
            .is_some());
        assert!(s.on_adapters("10.0.0.2", &list(&["10.0.0.2"])).is_none());
    }

    #[test]
    fn any_adapter_change_restarts_without_a_selection() {
        let mut s = Supervisor::default();
        assert!(s.on_adapters("", &list(&["10.0.0.2"])).is_none());
        assert!(s.on_adapters("", &list(&["10.0.0.2"])).is_none());
        assert!(s.on_adapters("", &list(&["10.0.0.3"])).is_some());
        // No adapters at all (cable out) is not a change worth a restart yet.
        assert!(s.on_adapters("", &[]).is_none());
    }

    #[test]
    fn client_retries_back_off() {
        let mut s = Supervisor::default();
        let t = Instant::now();
        assert!(
            s.on_link(false, t).is_none(),
            "never linked: nothing to restore"
        );
        assert!(s.on_link(true, t).is_none());
        assert!(s.on_link(false, t).is_none());
        assert!(s.on_link(false, t + Duration::from_millis(499)).is_none());
        assert!(s.on_link(false, t + Duration::from_millis(500)).is_some());
        assert!(s.on_link(false, t + Duration::from_millis(1499)).is_none());
        assert!(s.on_link(false, t + Duration::from_millis(1500)).is_some());
        assert!(s.on_link(false, t + Duration::from_millis(2999)).is_none());
        assert!(s.on_link(false, t + Duration::from_millis(3000)).is_some());
        assert!(s.on_link(true, t + Duration::from_millis(3001)).is_none());
        assert!(s.on_link(false, t + Duration::from_millis(3002)).is_none());
        assert!(s.on_link(false, t + Duration::from_millis(3502)).is_some());
    }
}
