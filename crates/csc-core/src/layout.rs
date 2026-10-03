use csc_proto::Edge;
use std::collections::HashMap;

#[derive(Debug, Default, Clone)]
pub struct ScreenLayout {
    graph: HashMap<String, HashMap<Edge, String>>,
}

impl ScreenLayout {
    pub fn load(layout: &csc_config::LayoutMap) -> Self {
        let mut graph = HashMap::new();
        for (host, edges) in layout {
            let mut m = HashMap::new();
            for (ename, neighbor) in edges {
                if let Some(e) = Edge::parse(ename) {
                    m.insert(e, neighbor.clone());
                }
            }
            graph.insert(host.clone(), m);
        }
        Self { graph }
    }

    pub fn neighbor(&self, host: &str, edge: Edge) -> Option<&str> {
        self.graph.get(host)?.get(&edge).map(|s| s.as_str())
    }

    /// True when `name` is a layout key or a neighbor of one (share scope).
    pub fn in_share_scope(&self, name: &str) -> bool {
        if self.graph.keys().any(|k| k.eq_ignore_ascii_case(name)) {
            return true;
        }
        self.graph
            .values()
            .any(|edges| edges.values().any(|n| n.eq_ignore_ascii_case(name)))
    }

    /// Reverse lookup: which edge of `from` points to `to`.
    pub fn edge_toward(&self, from: &str, to: &str) -> Option<Edge> {
        let edges = self.graph.get(from)?;
        for (e, n) in edges {
            if n.eq_ignore_ascii_case(to) {
                return Some(*e);
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    #[test]
    fn neighbor_right() {
        let mut layout = csc_config::LayoutMap::new();
        let mut edges = BTreeMap::new();
        edges.insert("right".into(), "B".into());
        layout.insert("A".into(), edges);
        let s = ScreenLayout::load(&layout);
        assert_eq!(s.neighbor("A", Edge::Right), Some("B"));
    }
}
