//! Layout nodes and stack sizing, ports of
//! `packages/tui/src/layout-node.ts` and `packages/tui/src/components/stack.ts`.

use std::sync::Arc;

use crate::tui::Component;

/// Layout viewport dimensions.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LayoutViewport {
    pub width: f64,
    pub height: f64,
}

#[derive(Clone)]
pub struct StackLayoutEntry {
    pub component: Arc<dyn Component>,
    pub basis: Option<f64>,
    pub grow: Option<f64>,
    pub shrink: Option<f64>,
    pub min_size: Option<f64>,
    pub max_size: Option<f64>,
    pub visible: Option<Arc<dyn Fn(&LayoutViewport) -> bool + Send + Sync>>,
}

impl std::fmt::Debug for StackLayoutEntry {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("StackLayoutEntry")
            .field("basis", &self.basis)
            .field("grow", &self.grow)
            .field("shrink", &self.shrink)
            .field("min_size", &self.min_size)
            .field("max_size", &self.max_size)
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum LayoutAlign {
    Stretch,
    Start,
    Center,
    End,
}

impl LayoutAlign {
    pub fn parse(value: &str) -> Self {
        match value {
            "start" => LayoutAlign::Start,
            "center" => LayoutAlign::Center,
            "end" => LayoutAlign::End,
            _ => LayoutAlign::Stretch,
        }
    }
}

#[derive(Clone)]
pub enum LayoutNode {
    Stack {
        kind: StackKind,
        entries: Vec<StackLayoutEntry>,
        gap: f64,
        align: LayoutAlign,
    },
    Scroll(ScrollLayoutNode),
}

impl std::fmt::Debug for LayoutNode {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LayoutNode::Stack { kind, gap, align, .. } => formatter
                .debug_struct("Stack")
                .field("kind", kind)
                .field("gap", gap)
                .field("align", align)
                .finish_non_exhaustive(),
            LayoutNode::Scroll(node) => formatter
                .debug_struct("Scroll")
                .field("scroll_top", &node.scroll_top)
                .finish_non_exhaustive(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum StackKind {
    VStack,
    HStack,
}

/// Scroll layout state (subset of ScrollView).
#[derive(Clone)]
pub struct ScrollLayoutNode {
    pub component: Arc<dyn Component>,
    pub scroll_top: f64,
    pub primary: bool,
    pub overscroll: String,
    pub viewport_height: f64,
}

impl StackLayoutEntry {
    pub fn new(component: Arc<dyn Component>) -> Self {
        Self {
            component,
            basis: None,
            grow: None,
            shrink: None,
            min_size: None,
            max_size: None,
            visible: None,
        }
    }

    pub fn with_basis(mut self, basis: f64) -> Self {
        self.basis = Some(basis);
        self
    }

    pub fn with_grow(mut self, grow: f64) -> Self {
        self.grow = Some(grow);
        self
    }

    pub fn with_shrink(mut self, shrink: f64) -> Self {
        self.shrink = Some(shrink);
        self
    }

    pub fn with_min_size(mut self, min_size: f64) -> Self {
        self.min_size = Some(min_size);
        self
    }

    pub fn with_max_size(mut self, max_size: f64) -> Self {
        self.max_size = Some(max_size);
        self
    }
}

fn normalize_size(value: Option<f64>, fallback: f64) -> f64 {
    match value {
        Some(value) if value.is_finite() => value.max(0.0).floor(),
        _ => fallback,
    }
}

/// Filter entries by visibility.
pub fn visible_stack_entries(entries: &[StackLayoutEntry], viewport: &LayoutViewport) -> Vec<StackLayoutEntry> {
    entries
        .iter()
        .filter(|entry| match &entry.visible {
            Some(visible) => visible(viewport),
            None => true,
        })
        .cloned()
        .collect()
}

fn clamp_size(size: f64, entry: &StackLayoutEntry) -> f64 {
    let min = entry.min_size.unwrap_or(0.0).max(0.0).floor();
    let max = entry
        .max_size
        .unwrap_or(f64::MAX)
        .max(min)
        .floor();
    size.max(0.0).floor().max(min).min(max)
}

fn distribute(
    sizes: &mut [f64],
    entries: &[StackLayoutEntry],
    amount: f64,
    mode: &str,
) {
    let mut remaining = amount;
    while remaining > 0.0 {
        let candidates: Vec<usize> = entries
            .iter()
            .enumerate()
            .filter(|(index, entry)| {
                if mode == "grow" {
                    entry.grow.unwrap_or(0.0) > 0.0 && sizes[*index] < entry.max_size.unwrap_or(f64::MAX)
                } else {
                    entry.shrink.unwrap_or(1.0) > 0.0 && sizes[*index] > entry.min_size.unwrap_or(0.0)
                }
            })
            .map(|(index, _)| index)
            .collect();
        if candidates.is_empty() {
            return;
        }

        let total_weight: f64 = candidates
            .iter()
            .map(|index| {
                let entry = &entries[*index];
                if mode == "grow" {
                    entry.grow.unwrap_or(0.0)
                } else {
                    entry.shrink.unwrap_or(1.0) * sizes[*index].max(1.0)
                }
            })
            .sum();
        let mut distributed = 0.0;
        for index in &candidates {
            if remaining <= 0.0 {
                break;
            }
            let entry = &entries[*index];
            let weight = if mode == "grow" {
                entry.grow.unwrap_or(0.0)
            } else {
                entry.shrink.unwrap_or(1.0) * sizes[*index].max(1.0)
            };
            let proposed = (remaining * weight / total_weight).max(1.0).floor();
            let capacity = if mode == "grow" {
                entry.max_size.unwrap_or(f64::MAX) - sizes[*index]
            } else {
                sizes[*index] - entry.min_size.unwrap_or(0.0)
            };
            let delta = remaining.min(proposed).min(capacity);
            if delta <= 0.0 {
                continue;
            }
            if mode == "grow" {
                sizes[*index] += delta;
            } else {
                sizes[*index] -= delta;
            }
            remaining -= delta;
            distributed += delta;
        }
        if distributed == 0.0 {
            return;
        }
    }
}

/// Allocate sizes for stack entries given intrinsic sizes and available
/// space.
pub fn allocate_stack_sizes(
    entries: &[StackLayoutEntry],
    intrinsic_sizes: &[f64],
    available_size: Option<f64>,
    gap: f64,
) -> Vec<f64> {
    let mut sizes: Vec<f64> = entries
        .iter()
        .enumerate()
        .map(|(index, entry)| {
            let base = match entry.basis {
                Some(basis) => basis,
                None => intrinsic_sizes.get(index).copied().unwrap_or(0.0),
            };
            clamp_size(base, entry)
        })
        .collect();
    let Some(available_size) = available_size else {
        return sizes;
    };

    let content_size = available_size.max(0.0).floor() - (entries.len().saturating_sub(1) as f64) * gap.max(0.0);
    let content_size = content_size.max(0.0);
    let total: f64 = sizes.iter().sum();
    if total < content_size {
        distribute(&mut sizes, entries, content_size - total, "grow");
    } else if total > content_size {
        distribute(&mut sizes, entries, total - content_size, "shrink");
    }
    sizes
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FixedComponent {
        height: usize,
    }

    impl Component for FixedComponent {
        fn render(&self, _width: usize) -> Vec<String> {
            vec![String::new(); self.height]
        }
    }

    fn entry(height: usize) -> StackLayoutEntry {
        StackLayoutEntry::new(Arc::new(FixedComponent { height }))
    }

    #[test]
    fn sizes_match_intrinsic_when_no_constraint() {
        let entries = vec![entry(3), entry(2)];
        let sizes = allocate_stack_sizes(&entries, &[3.0, 2.0], None, 0.0);
        assert_eq!(sizes, vec![3.0, 2.0]);
    }

    #[test]
    fn grows_to_fill_available() {
        // The JS distribute loop assigns per candidate with a floor(max(1))
        // step, so equal weights do not split evenly across rounds.
        let entries = vec![entry(1).with_grow(1.0), entry(1).with_grow(1.0)];
        let sizes = allocate_stack_sizes(&entries, &[1.0, 1.0], Some(10.0), 0.0);
        assert_eq!(sizes.iter().sum::<f64>(), 10.0);
        assert_eq!(sizes, vec![6.0, 4.0]);
    }

    #[test]
    fn grows_with_weights() {
        let entries = vec![entry(1).with_grow(1.0), entry(1).with_grow(2.0)];
        let sizes = allocate_stack_sizes(&entries, &[1.0, 1.0], Some(6.0), 0.0);
        assert_eq!(sizes.iter().sum::<f64>(), 6.0);
        // Round-based distribution: [3, 3] with the JS max(1) step.
        assert_eq!(sizes, vec![3.0, 3.0]);
    }

    #[test]
    fn shrinks_when_overconstrained() {
        let entries = vec![entry(5), entry(5)];
        let sizes = allocate_stack_sizes(&entries, &[5.0, 5.0], Some(6.0), 0.0);
        assert_eq!(sizes.iter().sum::<f64>(), 6.0);
        // Round-based distribution with max(1) steps: [2, 4].
        assert_eq!(sizes, vec![2.0, 4.0]);
    }

    #[test]
    fn gap_reduces_content_space() {
        let entries = vec![entry(2).with_grow(1.0), entry(2).with_grow(1.0)];
        let sizes = allocate_stack_sizes(&entries, &[2.0, 2.0], Some(10.0), 2.0);
        // Content size = 10 - 2 = 8; total 4 grows by 4.
        assert_eq!(sizes.iter().sum::<f64>(), 8.0);
    }

    #[test]
    fn basis_and_clamps() {
        // Entries without grow never expand (JS `grow ?? 0`).
        let entries = vec![entry(1).with_basis(4.0).with_min_size(3.0), entry(1)];
        let sizes = allocate_stack_sizes(&entries, &[1.0, 1.0], Some(10.0), 0.0);
        assert_eq!(sizes[0], 4.0);
        assert_eq!(sizes[1], 1.0);

        let entries = vec![entry(1).with_max_size(2.0).with_grow(1.0), entry(1).with_grow(1.0)];
        let sizes = allocate_stack_sizes(&entries, &[1.0, 1.0], Some(10.0), 0.0);
        assert_eq!(sizes[0], 2.0);
        assert_eq!(sizes[1], 8.0);
    }

    #[test]
    fn visible_entries_filter() {
        let mut entries = vec![entry(1), entry(1)];
        entries[1].visible = Some(Arc::new(|viewport: &LayoutViewport| viewport.width > 100.0));
        let visible = visible_stack_entries(&entries, &LayoutViewport {
            width: 50.0,
            height: 10.0,
        });
        assert_eq!(visible.len(), 1);
        let visible = visible_stack_entries(&entries, &LayoutViewport {
            width: 150.0,
            height: 10.0,
        });
        assert_eq!(visible.len(), 2);
    }
}
