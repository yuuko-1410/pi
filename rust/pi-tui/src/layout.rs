//! Layout engine, port of `packages/tui/src/layout.ts`.
//!
//! Renders a component tree into a screen of lines with clipping,
//! scrollbars, and scroll view integration.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use crate::components::scroll_view::ScrollView;
use crate::layout_node::{LayoutAlign, LayoutNode, StackKind};
use crate::terminal_image::{crop_kitty_image_line, get_kitty_image_metadata, is_image_line};
use crate::tui::{composite_tui_line, Component, CURSOR_MARKER};
use crate::utils::{extract_ansi_code, get_grapheme_cell_range, slice_by_column, visible_width};

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LayoutRect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

#[derive(Clone)]
pub struct LayoutBox {
    pub component: Arc<dyn Component>,
    pub rect: LayoutRect,
    pub clip: LayoutRect,
    pub children: Vec<LayoutBox>,
    pub parent: Option<usize>,
    pub lines: Option<Vec<String>>,
    pub line_offset: f64,
    pub scroll_view: Option<u64>,
    pub scroll_content_lines: Option<Vec<String>>,
    pub layer: f64,
}

pub struct LayoutFrame {
    pub root: LayoutBox,
    pub width: f64,
    pub height: f64,
    pub lines: Vec<String>,
    pub primary_scroll_view: Option<u64>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ScrollbarGeometry {
    pub column: f64,
    pub track_top: f64,
    pub track_height: f64,
    pub thumb_top: f64,
    pub thumb_height: f64,
    pub max_scroll_top: f64,
}

pub struct LayoutContext {
    pub viewport: (f64, f64),
    pub render_cache: Mutex<HashMap<usize, HashMap<usize, Vec<String>>>>,
    pub scroll_states: HashMap<u64, Arc<Mutex<ScrollView>>>,
    /// Layout node registration table keyed by component pointer.
    pub nodes: HashMap<usize, LayoutNode>,
}

fn intersect(a: LayoutRect, b: LayoutRect) -> LayoutRect {
    let x = a.x.max(b.x);
    let y = a.y.max(b.y);
    let right = (a.x + a.width).min(b.x + b.width);
    let bottom = (a.y + a.height).min(b.y + b.height);
    LayoutRect {
        x,
        y,
        width: (right - x).max(0.0),
        height: (bottom - y).max(0.0),
    }
}

fn render_cached(context: &LayoutContext, component: &Arc<dyn Component>, width: f64) -> Vec<String> {
    let safe_width = width.max(1.0).floor() as usize;
    let key = Arc::as_ptr(component) as *const () as usize;
    let mut cache = context.render_cache.lock().unwrap();
    let widths = cache.entry(key).or_default();
    if let Some(lines) = widths.get(&safe_width) {
        return lines.clone();
    }
    let lines = component.render(safe_width);
    widths.insert(safe_width, lines.clone());
    lines
}

fn measure_height(context: &LayoutContext, component: &Arc<dyn Component>, width: f64) -> f64 {
    render_cached(context, component, width).len() as f64
}

fn measure_width(context: &LayoutContext, component: &Arc<dyn Component>, width: f64) -> f64 {
    render_cached(context, component, width)
        .iter()
        .map(|line| visible_width(line))
        .fold(0.0, f64::max)
}

fn layout_component(
    context: &LayoutContext,
    component: &Arc<dyn Component>,
    x: f64,
    y: f64,
    width: f64,
    height: Option<f64>,
    clip: LayoutRect,
    node: Option<&LayoutNode>,
) -> LayoutBox {
    let safe_width = width.max(1.0).floor();

    let Some(node) = node else {
        let lines = render_cached(context, component, safe_width);
        let allocated_height = match height {
            Some(height) => height.max(0.0).floor(),
            None => lines.len() as f64,
        };
        let mut line_offset = 0.0;
        if lines.len() as f64 > allocated_height && allocated_height > 0.0 {
            if let Some(cursor_line) = lines.iter().position(|line| line.contains(CURSOR_MARKER)) {
                if cursor_line as f64 >= allocated_height {
                    line_offset = cursor_line as f64 - allocated_height + 1.0;
                }
            }
        }
        return LayoutBox {
            component: component.clone(),
            rect: LayoutRect {
                x,
                y,
                width: safe_width,
                height: allocated_height,
            },
            clip: intersect(
                clip,
                LayoutRect {
                    x,
                    y,
                    width: safe_width,
                    height: allocated_height,
                },
            ),
            children: Vec::new(),
            parent: None,
            lines: Some(lines),
            line_offset,
            scroll_view: None,
            scroll_content_lines: None,
            layer: 0.0,
        };
    };

    match node {
        LayoutNode::Scroll(scroll_node) => {
            let scroll_id = context
                .scroll_states
                .iter()
                .find(|(_, state)| {
                    let inner = state.lock().unwrap();
                    Arc::ptr_eq(
                        inner.child_ref(),
                        &scroll_node.component,
                    )
                })
                .map(|(id, _)| *id)
                .or_else(|| {
                    // Fallback: match by first scroll state (single scroll view).
                    context.scroll_states.keys().next().copied()
                });
            let scroll_id = match scroll_id {
                Some(id) => id,
                None => {
                    // No registered scroll view; render child directly.
                    return layout_component(context, &scroll_node.component, x, y, width, height, clip, None);
                }
            };
            let previous_scroll_top = {
                let state = context.scroll_states.get(&scroll_id).unwrap();
                let inner = state.lock().unwrap();
                inner.scroll_top()
            };
            let content_width = {
                let state = context.scroll_states.get(&scroll_id).unwrap();
                let inner = state.lock().unwrap();
                inner.get_content_width(safe_width)
            };
            let child_box = layout_component(
                context,
                &scroll_node.component,
                x,
                y - previous_scroll_top,
                content_width,
                None,
                clip,
                None,
            );
            let content_height = child_box.rect.height;
            let viewport_height = match height {
                Some(height) => height.max(0.0).floor(),
                None => content_height,
            };
            {
                let state = context.scroll_states.get(&scroll_id).unwrap();
                let mut inner = state.lock().unwrap();
                inner.update_layout(content_height, viewport_height, Arc::new(|| {}));
            }
            let current_scroll_top = {
                let state = context.scroll_states.get(&scroll_id).unwrap();
                let inner = state.lock().unwrap();
                inner.scroll_top()
            };
            let rect = LayoutRect {
                x,
                y,
                width: safe_width,
                height: viewport_height,
            };
            let child_clip = intersect(clip, rect);
            let scroll_content_lines = render_cached(context, &scroll_node.component, content_width);
            LayoutBox {
                component: component.clone(),
                rect,
                clip: child_clip,
                children: vec![child_box],
                parent: None,
                lines: None,
                line_offset: current_scroll_top,
                scroll_view: Some(scroll_id),
                scroll_content_lines: Some(scroll_content_lines),
                layer: 0.0,
            }
        }
        LayoutNode::Stack { kind, entries, gap, align } => {
            let visible_entries: Vec<&crate::layout_node::StackLayoutEntry> = entries
                .iter()
                .filter(|entry| match &entry.visible {
                    Some(visible) => visible(&crate::layout_node::LayoutViewport {
                        width: context.viewport.0,
                        height: context.viewport.1,
                    }),
                    None => true,
                })
                .collect();
            let gap_total = (visible_entries.len().saturating_sub(1) as f64) * gap.max(0.0);
            let rect = LayoutRect {
                x,
                y,
                width: safe_width,
                height: height.unwrap_or(0.0).max(0.0).floor(),
            };
            let mut layout_box = LayoutBox {
                component: component.clone(),
                rect,
                clip: intersect(clip, rect),
                children: Vec::new(),
                parent: None,
                lines: None,
                line_offset: 0.0,
                scroll_view: None,
                scroll_content_lines: None,
                layer: 0.0,
            };

            match kind {
                StackKind::VStack => {
                    let intrinsic_heights: Vec<f64> = visible_entries
                        .iter()
                        .map(|entry| match entry.basis {
                            Some(basis) => basis,
                            None => measure_height(context, &entry.component, safe_width),
                        })
                        .collect();
                    let sizes = crate::layout_node::allocate_stack_sizes(
                        entries,
                        &intrinsic_heights,
                        height,
                        gap.max(0.0),
                    );
                    let natural_height: f64 = sizes.iter().sum::<f64>() + gap_total;
                    let allocated_height = match height {
                        Some(height) => height.max(0.0).floor(),
                        None => natural_height,
                    };
                    layout_box.rect.height = allocated_height;
                    layout_box.clip = intersect(clip, layout_box.rect);
                    let mut child_y = y;
                    for (index, entry) in visible_entries.iter().enumerate() {
                        let child = layout_component(
                            context,
                            &entry.component,
                            x,
                            child_y,
                            safe_width,
                            Some(sizes[index]),
                            layout_box.clip,
                            layout_node_of(context, &entry.component).as_ref(),
                        );
                        layout_box.children.push(child);
                        child_y += sizes[index] + gap.max(0.0);
                    }
                }
                StackKind::HStack => {
                    let intrinsic_widths: Vec<f64> = visible_entries
                        .iter()
                        .map(|entry| match entry.basis {
                            Some(basis) => basis,
                            None => measure_width(context, &entry.component, safe_width),
                        })
                        .collect();
                    let widths = crate::layout_node::allocate_stack_sizes(
                        entries,
                        &intrinsic_widths,
                        Some(safe_width),
                        gap.max(0.0),
                    );
                    let intrinsic_heights: Vec<f64> = visible_entries
                        .iter()
                        .enumerate()
                        .map(|(index, entry)| {
                            measure_height(context, &entry.component, widths[index].max(1.0))
                        })
                        .collect();
                    let allocated_height = match height {
                        Some(height) => height.max(0.0).floor(),
                        None => intrinsic_heights
                            .iter()
                            .fold(0.0f64, |max, child_height| max.max(*child_height)),
                    };
                    layout_box.rect.height = allocated_height;
                    layout_box.clip = intersect(clip, layout_box.rect);
                    let mut child_x = x;
                    for (index, entry) in visible_entries.iter().enumerate() {
                        let natural_child_height = intrinsic_heights[index];
                        let child_height = match align {
                            LayoutAlign::Stretch => allocated_height,
                            _ => allocated_height.min(natural_child_height),
                        };
                        let mut child_y = y;
                        match align {
                            LayoutAlign::Center => {
                                child_y += ((allocated_height - child_height) / 2.0).floor()
                            }
                            LayoutAlign::End => child_y += allocated_height - child_height,
                            _ => {}
                        }
                        let child_width = widths[index];
                        if child_width == 0.0 {
                            layout_box.children.push(LayoutBox {
                                component: entry.component.clone(),
                                rect: LayoutRect {
                                    x: child_x,
                                    y: child_y,
                                    width: 0.0,
                                    height: child_height,
                                },
                                clip: LayoutRect {
                                    x: child_x,
                                    y: child_y,
                                    width: 0.0,
                                    height: 0.0,
                                },
                                children: Vec::new(),
                                parent: None,
                                lines: None,
                                line_offset: 0.0,
                                scroll_view: None,
                                scroll_content_lines: None,
                                layer: 0.0,
                            });
                        } else {
                            layout_box.children.push(layout_component(
                                context,
                                &entry.component,
                                child_x,
                                child_y,
                                child_width,
                                Some(child_height),
                                layout_box.clip,
                                layout_node_of(context, &entry.component).as_ref(),
                            ));
                        }
                        child_x += child_width + gap.max(0.0);
                    }
                }
            }
            layout_box
        }
    }
}

/// Layout-aware components expose their layout node (JS `[LAYOUT_NODE]()`).
pub trait LayoutAware: Component {
    fn layout_node(&self) -> LayoutNode;
}

/// Look up the layout node for a component from the context registration
/// table.
fn layout_node_of(context: &LayoutContext, component: &Arc<dyn Component>) -> Option<LayoutNode> {
    context
        .nodes
        .get(&(Arc::as_ptr(component) as *const () as usize))
        .cloned()
}

fn style_scrollbar_cell(
    line: &str,
    column: f64,
    total_width: f64,
    style: &dyn Fn(&str) -> String,
) -> String {
    if is_image_line(line) {
        return line.to_string();
    }
    let grapheme_range = get_grapheme_cell_range(line, column);
    let start = grapheme_range.map(|range| range.0).unwrap_or(column);
    let end = grapheme_range.map(|range| range.1).unwrap_or(column + 1.0);
    let before = slice_by_column(line, 0.0, start, true);
    let target = slice_by_column(line, start, end - start, true);
    let after = slice_by_column(line, end, (total_width - end).max(0.0), true);

    let mut target_prefix = String::new();
    let mut target_index = 0;
    while target_index < target.len() {
        if let Some(ansi) = extract_ansi_code(&target, target_index) {
            target_prefix += &ansi.code;
            target_index += ansi.length;
        } else {
            break;
        }
    }
    let target_text = if target[target_index..].is_empty() {
        " ".repeat((end - start) as usize)
    } else {
        target[target_index..].to_string()
    };
    let before_padding = " ".repeat(((start - visible_width(&before)).max(0.0)) as usize);
    format!("{before}{before_padding}{target_prefix}{}{after}", style(&target_text))
}

/// Compute scrollbar geometry for a box.
pub fn get_scrollbar_geometry(layout_box: &LayoutBox, scroll_view_visible: bool) -> Option<ScrollbarGeometry> {
    if !scroll_view_visible || layout_box.rect.width <= 0.0 || layout_box.rect.height <= 0.0 {
        return None;
    }
    let content_height = layout_box
        .children
        .first()
        .map(|child| child.rect.height)
        .or_else(|| layout_box.scroll_content_lines.as_ref().map(|lines| lines.len() as f64))
        .unwrap_or(0.0);
    let track_height = layout_box.rect.height;
    let min_thumb_height = 2.0f64.min(track_height);
    let thumb_height = min_thumb_height.max(
        (track_height * track_height / content_height).round().min(track_height),
    );
    let max_scroll_top = (content_height - track_height).max(0.0);
    let max_thumb_top = track_height - thumb_height;
    let thumb_offset = if max_scroll_top == 0.0 {
        0.0
    } else {
        ((layout_box.line_offset / max_scroll_top) * max_thumb_top).round()
    };
    let column = layout_box.rect.x + layout_box.rect.width - 1.0;
    if column < layout_box.clip.x || column >= layout_box.clip.x + layout_box.clip.width {
        return None;
    }
    Some(ScrollbarGeometry {
        column,
        track_top: layout_box.rect.y,
        track_height,
        thumb_top: layout_box.rect.y + thumb_offset,
        thumb_height,
        max_scroll_top,
    })
}

fn paint_scrollbar(
    layout_box: &LayoutBox,
    screen: &mut [String],
    total_width: f64,
    scroll_view: &ScrollView,
) {
    let visible = scroll_view.is_scrollbar_visible();
    let Some(geometry) = get_scrollbar_geometry(layout_box, visible) else {
        return;
    };
    let style = scroll_view.scrollbar_style.clone();
    for offset in 0..geometry.thumb_height as usize {
        let row = (geometry.thumb_top + offset as f64) as usize;
        if row < layout_box.clip.y as usize
            || row >= (layout_box.clip.y + layout_box.clip.height) as usize
            || row >= screen.len()
        {
            continue;
        }
        let line = screen.get(row).cloned().unwrap_or_default();
        screen[row] = style_scrollbar_cell(&line, geometry.column, total_width, &*style);
    }
}

fn paint_box(layout_box: &LayoutBox, screen: &mut [String], total_width: f64, scroll_states: &HashMap<u64, Arc<Mutex<ScrollView>>>) {
    if let Some(lines) = &layout_box.lines {
        let offset = layout_box.line_offset as usize;
        let first_row = (layout_box.rect.y.max(layout_box.clip.y).max(0.0)) as usize;
        let last_row = ((layout_box.rect.y + layout_box.rect.height)
            .min(layout_box.clip.y + layout_box.clip.height)
            .min(screen.len() as f64)) as usize;
        for row in first_row..last_row {
            let source_index = offset + row - layout_box.rect.y as usize;
            let Some(source_line) = lines.get(source_index) else {
                continue;
            };
            let mut line = strip_osc133_zones(source_line);
            let image_metadata = get_kitty_image_metadata(&line);
            if image_metadata.is_some() {
                let clip_bottom = (screen.len() as f64).min(layout_box.clip.y + layout_box.clip.height);
                let visible_rows = image_metadata.as_ref().unwrap().rows.min(clip_bottom - row as f64);
                if visible_rows < image_metadata.as_ref().unwrap().rows {
                    line = crop_kitty_image_line(&line, 0.0, visible_rows);
                }
            }
            if layout_box.rect.x == 0.0 && layout_box.rect.width >= total_width && (is_image_line(&line) || screen[row].is_empty()) {
                screen[row] = line;
            } else {
                screen[row] = composite_tui_line(
                    &screen[row],
                    &line,
                    layout_box.rect.x,
                    layout_box.rect.width,
                    total_width,
                );
            }
        }
    }
    for child in &layout_box.children {
        paint_box(child, screen, total_width, scroll_states);
    }
    if let Some(scroll_id) = layout_box.scroll_view {
        if let Some(state) = scroll_states.get(&scroll_id) {
            let inner = state.lock().unwrap();
            if layout_box.rect.height > 0.0 {
                paint_scrollbar(layout_box, screen, total_width, &inner);
            }
        }
    }
}

/// Strip OSC 133 prompt zones (ESC ] 133;A/B/C ... BEL/ST).
fn strip_osc133_zones(line: &str) -> String {
    if !line.contains("\x1b]133;") {
        return line.to_string();
    }
    let mut result = String::new();
    let mut i = 0;
    while i < line.len() {
        if line[i..].starts_with("\x1b]133;") {
            // Find terminator.
            let rest = &line[i + 7..];
            let terminator = rest.find('\x07').map(|index| index + 1).or_else(|| rest.find("\x1b\\").map(|index| index + 2));
            match terminator {
                Some(length) => {
                    i += 7 + length;
                }
                None => {
                    result.push_str(&line[i..]);
                    break;
                }
            }
        } else {
            let char = line[i..].chars().next().unwrap();
            result.push(char);
            i += char.len_utf8();
        }
    }
    result
}

/// Render a component tree into a layout frame.
pub fn render_layout_frame(
    root: &Arc<dyn Component>,
    width: f64,
    height: f64,
    scroll_states: HashMap<u64, Arc<Mutex<ScrollView>>>,
    nodes: HashMap<usize, LayoutNode>,
) -> LayoutFrame {
    let safe_width = width.max(1.0).floor();
    let safe_height = height.max(1.0).floor();
    let mut context = LayoutContext {
        viewport: (safe_width, safe_height),
        render_cache: Mutex::new(HashMap::new()),
        scroll_states,
        nodes,
    };
    let root_node = layout_node_of(&context, root);
    let root_box = layout_component(
        &mut context,
        root,
        0.0,
        0.0,
        safe_width,
        Some(safe_height),
        LayoutRect {
            x: 0.0,
            y: 0.0,
            width: safe_width,
            height: safe_height,
        },
        root_node.as_ref(),
    );
    let mut lines = vec![String::new(); safe_height as usize];
    paint_box(&root_box, &mut lines, safe_width, &context.scroll_states);
    let primary_scroll_view = context
        .scroll_states
        .iter()
        .find(|(_, state)| state.lock().unwrap().primary)
        .map(|(id, _)| *id);
    LayoutFrame {
        root: root_box,
        width: safe_width,
        height: safe_height,
        lines,
        primary_scroll_view,
    }
}

fn contains_point(rect: LayoutRect, x: f64, y: f64) -> bool {
    x >= rect.x && x < rect.x + rect.width && y >= rect.y && y < rect.y + rect.height
}

/// Find the box containing a scroll view.
pub fn get_scroll_view_box(frame: &LayoutFrame, scroll_id: u64) -> Option<LayoutBox> {
    fn visit(node: &LayoutBox, scroll_id: u64) -> Option<LayoutBox> {
        if node.scroll_view == Some(scroll_id) {
            return Some(node.clone());
        }
        for child in &node.children {
            if let Some(matched) = visit(child, scroll_id) {
                return Some(matched);
            }
        }
        None
    }
    visit(&frame.root, scroll_id)
}

/// Collect scroll views at a point, deepest first.
pub fn get_scroll_views_at(frame: &LayoutFrame, x: f64, y: f64) -> Vec<u64> {
    let mut result: Vec<(u64, usize)> = Vec::new();
    fn visit(node: &LayoutBox, x: f64, y: f64, depth: usize, result: &mut Vec<(u64, usize)>) {
        if !contains_point(node.clip, x, y) {
            return;
        }
        if let Some(scroll_id) = node.scroll_view {
            if contains_point(node.rect, x, y) {
                result.push((scroll_id, depth));
            }
        }
        for child in &node.children {
            visit(child, x, y, depth + 1, result);
        }
    }
    visit(&frame.root, x, y, 0, &mut result);
    result.sort_by(|a, b| b.1.cmp(&a.1));
    result.into_iter().map(|(id, _)| id).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout_node::StackLayoutEntry;

    struct TextChild {
        text: String,
    }

    impl Component for TextChild {
        fn render(&self, _width: usize) -> Vec<String> {
            vec![self.text.clone()]
        }
    }

    #[test]
    fn intersects_rects() {
        let a = LayoutRect {
            x: 0.0,
            y: 0.0,
            width: 10.0,
            height: 10.0,
        };
        let b = LayoutRect {
            x: 5.0,
            y: 5.0,
            width: 10.0,
            height: 10.0,
        };
        assert_eq!(
            intersect(a, b),
            LayoutRect {
                x: 5.0,
                y: 5.0,
                width: 5.0,
                height: 5.0
            }
        );
        let c = LayoutRect {
            x: 20.0,
            y: 20.0,
            width: 5.0,
            height: 5.0,
        };
        assert_eq!(intersect(a, c).width, 0.0);
    }

    #[test]
    fn renders_leaf_component() {
        let component: Arc<dyn Component> = Arc::new(TextChild {
            text: "hello".to_string(),
        });
        let frame = render_layout_frame(&component, 10.0, 5.0, HashMap::new(), HashMap::new());
        assert_eq!(frame.lines.len(), 5);
        assert_eq!(frame.lines[0], "hello");
        assert_eq!(frame.root.rect.width, 10.0);
        assert_eq!(frame.root.rect.height, 5.0);
    }

    #[test]
    fn vstack_lays_out_children() {
        let a: Arc<dyn Component> = Arc::new(TextChild {
            text: "aa".to_string(),
        });
        let b: Arc<dyn Component> = Arc::new(TextChild {
            text: "bb".to_string(),
        });
        let entry_a = StackLayoutEntry::new(a).with_grow(1.0);
        let entry_b = StackLayoutEntry::new(b).with_grow(1.0);
        let vstack = Arc::new(VStackComponent {
            entries: vec![entry_a, entry_b],
            gap: 0.0,
            align: LayoutAlign::Stretch,
        });
        let mut nodes = HashMap::new();
        nodes.insert(
            Arc::as_ptr(&vstack) as *const () as usize,
            LayoutNode::Stack {
                kind: StackKind::VStack,
                entries: vstack.entries.clone(),
                gap: 0.0,
                align: LayoutAlign::Stretch,
            },
        );
        let root: Arc<dyn Component> = vstack;
        let frame = render_layout_frame(&root, 10.0, 4.0, HashMap::new(), nodes);
        assert!(frame.lines[0].contains("aa"));
        assert!(frame.lines[2].contains("bb"));
    }

    #[allow(dead_code)]
    struct VStackComponent {
        entries: Vec<StackLayoutEntry>,
        gap: f64,
        align: LayoutAlign,
    }

    impl Component for VStackComponent {
        fn render(&self, _width: usize) -> Vec<String> {
            vec![String::new(); 2]
        }
    }

    #[test]
    fn scrollbar_geometry() {
        let layout_box = LayoutBox {
            component: Arc::new(TextChild {
                text: "".to_string(),
            }),
            rect: LayoutRect {
                x: 0.0,
                y: 0.0,
                width: 10.0,
                height: 5.0,
            },
            clip: LayoutRect {
                x: 0.0,
                y: 0.0,
                width: 10.0,
                height: 5.0,
            },
            children: vec![LayoutBox {
                component: Arc::new(TextChild {
                    text: "".to_string(),
                }),
                rect: LayoutRect {
                    x: 0.0,
                    y: 0.0,
                    width: 10.0,
                    height: 20.0,
                },
                clip: LayoutRect {
                    x: 0.0,
                    y: 0.0,
                    width: 10.0,
                    height: 5.0,
                },
                children: Vec::new(),
                parent: None,
                lines: None,
                line_offset: 3.0,
                scroll_view: None,
                scroll_content_lines: None,
                layer: 0.0,
            }],
            parent: None,
            lines: None,
            line_offset: 3.0,
            scroll_view: None,
            scroll_content_lines: None,
            layer: 0.0,
        };
        let geometry = get_scrollbar_geometry(&layout_box, true).unwrap();
        assert_eq!(geometry.column, 9.0);
        assert_eq!(geometry.track_height, 5.0);
        assert_eq!(geometry.max_scroll_top, 15.0);
        assert!(geometry.thumb_top >= 0.0);
    }
}
