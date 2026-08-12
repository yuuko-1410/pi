//! LaTeX rendering, port of `packages/tui/src/latex.ts`.
//!
//! Full parser state machine with symbol tables, fractions/operators/matrices
//! as layout nodes, and display math vertical stacking. Tables are generated
//! from the JS source (see data module below).

pub const LAYOUT_MARKER_START: char = '\u{f0000}';
pub const LAYOUT_MARKER_END: char = '\u{f0001}';
const PROTECTED_SPACE: char = '\u{f0002}';
const NEGATIVE_SPACE: char = '\u{0}';
const NAMED_OPERATOR_START: char = '\u{f0004}';
const NAMED_OPERATOR_END: char = '\u{f0005}';

#[derive(Clone, Debug, PartialEq)]
pub enum LayoutNode {
    Fraction {
        numerator: String,
        denominator: String,
    },
    Operator {
        operator: String,
        lower: Option<String>,
        upper: Option<String>,
    },
    Matrix {
        lines: Vec<String>,
        baseline: f64,
    },
}

#[derive(Clone, Debug)]
struct Layout {
    lines: Vec<String>,
    width: f64,
    baseline: f64,
}

fn symbol_lookup<'a>(table: &'a [(&'a str, &'a str)], key: &str) -> Option<&'a str> {
    table.iter().find(|(k, _)| *k == key).map(|(_, v)| *v)
}

fn set_contains(set: &[&str], value: &str) -> bool {
    set.contains(&value)
}

fn visible_width_of(text: &str) -> f64 {
    crate::utils::visible_width(text)
}

fn pad_layout_line(line: &str, width: f64, centered: bool) -> String {
    let padding = (width - visible_width_of(line)).max(0.0);
    let left = if centered { (padding / 2.0).floor() } else { 0.0 };
    format!(
        "{}{}{}",
        " ".repeat(left as usize),
        line,
        " ".repeat((padding - left) as usize)
    )
}

fn join_layouts(layouts: &[Layout]) -> Layout {
    if layouts.is_empty() {
        return Layout {
            lines: vec![String::new()],
            width: 0.0,
            baseline: 0.0,
        };
    }
    let baseline = layouts
        .iter()
        .map(|layout| layout.baseline)
        .fold(0.0, f64::max);
    let below = layouts
        .iter()
        .map(|layout| layout.lines.len() as f64 - layout.baseline - 1.0)
        .fold(0.0, f64::max);
    let mut lines: Vec<String> = Vec::new();
    for row in 0..=(baseline + below) as usize {
        let mut line = String::new();
        for layout in layouts {
            let source_row = row as f64 - baseline + layout.baseline;
            if source_row >= 0.0 && source_row < layout.lines.len() as f64 {
                line += &pad_layout_line(&layout.lines[source_row as usize], layout.width, false);
            } else {
                line += &" ".repeat(layout.width as usize);
            }
        }
        lines.push(line.trim_end().to_string());
    }
    Layout {
        lines,
        width: layouts.iter().map(|layout| layout.width).sum(),
        baseline,
    }
}

fn render_layout(source: &str, nodes: &[LayoutNode]) -> Layout {
    let mut rendered_lines: Vec<String> = Vec::new();
    let mut first_baseline = 0.0f64;
    for source_line in source.split('\n') {
        let mut layouts: Vec<Layout> = Vec::new();
        let mut position = 0usize;
        let mut previous_node: Option<&LayoutNode> = None;
        let mut search_from = 0;
        while let Some(marker_start) = source_line[search_from..].find(LAYOUT_MARKER_START) {
            let index = search_from + marker_start;
            // Read the index between the markers (marker chars are
            // multi-byte, so advance by char counts).
            let marker_start_len = LAYOUT_MARKER_START.len_utf8();
            let after = &source_line[index + marker_start_len..];
            let Some(end) = after.find(LAYOUT_MARKER_END) else { break };
            let node_index = after[..end].parse::<usize>().ok();
            let Some(node_index) = node_index else { break };
            let marker_end = index + marker_start_len + end + LAYOUT_MARKER_END.len_utf8();
            let Some(node) = nodes.get(node_index) else {
                search_from = marker_end;
                continue;
            };
            if index > position {
                let sliced = &source_line[position..index];
                let trimmed_start = if previous_node.is_some() {
                    sliced.trim_start()
                } else {
                    sliced
                };
                let trimmed = trimmed_start.trim_end();
                let preserve_leading_space = matches!(previous_node, Some(LayoutNode::Matrix { .. }))
                    && sliced.starts_with(' ');
                let preserve_trailing_space = matches!(node, LayoutNode::Matrix { .. })
                    && sliced.ends_with(' ');
                let text = if !trimmed.is_empty() {
                    let mut text = String::new();
                    if preserve_leading_space {
                        text.push(' ');
                    }
                    text.push_str(trimmed);
                    if preserve_trailing_space {
                        text.push(' ');
                    }
                    text
                } else if preserve_leading_space || preserve_trailing_space {
                    " ".to_string()
                } else {
                    String::new()
                };
                layouts.push(Layout {
                    lines: vec![text.clone()],
                    width: visible_width_of(&text),
                    baseline: 0.0,
                });
            }
            match node {
                LayoutNode::Fraction { numerator, denominator } => {
                    let numerator_layout = render_layout(numerator, nodes);
                    let denominator_layout = render_layout(denominator, nodes);
                    let content_width = numerator_layout
                        .width
                        .max(denominator_layout.width)
                        .max(1.0);
                    let width = content_width + 2.0;
                    let mut lines: Vec<String> = numerator_layout
                        .lines
                        .iter()
                        .map(|line| pad_layout_line(line, width, true))
                        .collect();
                    lines.push(format!(" {} ", "─".repeat(content_width as usize)));
                    lines.extend(
                        denominator_layout
                            .lines
                            .iter()
                            .map(|line| pad_layout_line(line, width, true)),
                    );
                    layouts.push(Layout {
                        lines,
                        width,
                        baseline: numerator_layout.lines.len() as f64,
                    });
                }
                LayoutNode::Operator { operator, lower, upper } => {
                    let content_width = visible_width_of(operator)
                        .max(lower.as_ref().map(|v| visible_width_of(v)).unwrap_or(0.0))
                        .max(upper.as_ref().map(|v| visible_width_of(v)).unwrap_or(0.0));
                    let mut lines: Vec<String> = Vec::new();
                    if upper.is_some() {
                        lines.push(format!(
                            "{} ",
                            pad_layout_line(upper.as_ref().unwrap(), content_width, true)
                        ));
                    }
                    lines.push(format!(
                        "{} ",
                        pad_layout_line(operator, content_width, true)
                    ));
                    if lower.is_some() {
                        lines.push(format!(
                            "{} ",
                            pad_layout_line(lower.as_ref().unwrap(), content_width, true)
                        ));
                    }
                    layouts.push(Layout {
                        lines,
                        width: content_width + 1.0,
                        baseline: if upper.is_some() { 1.0 } else { 0.0 },
                    });
                }
                LayoutNode::Matrix { lines, baseline } => {
                    let width = lines
                        .iter()
                        .map(|line| visible_width_of(line))
                        .fold(0.0, f64::max);
                    layouts.push(Layout {
                        lines: lines
                            .iter()
                            .map(|line| pad_layout_line(line, width, false))
                            .collect(),
                        width,
                        baseline: *baseline,
                    });
                }
            }
            position = marker_end;
            previous_node = Some(node);
            search_from = marker_end;
        }
        if position < source_line.len() {
            let sliced = &source_line[position..];
            let trimmed = if previous_node.is_some() {
                sliced.trim_start()
            } else {
                sliced
            };
            let text = if matches!(previous_node, Some(LayoutNode::Matrix { .. })) && sliced.starts_with(' ') {
                format!(" {trimmed}")
            } else {
                trimmed.to_string()
            };
            layouts.push(Layout {
                lines: vec![text.clone()],
                width: visible_width_of(&text),
                baseline: 0.0,
            });
        }
        let line_layout = join_layouts(&layouts);
        if rendered_lines.is_empty() {
            first_baseline = line_layout.baseline;
        }
        rendered_lines.extend(line_layout.lines);
    }
    Layout {
        width: rendered_lines
            .iter()
            .map(|line| visible_width_of(line))
            .fold(0.0, f64::max),
        lines: rendered_lines,
        baseline: first_baseline,
    }
}

fn format_script(value: &str, kind: &str) -> String {
    if value.is_empty() {
        return String::new();
    }
    let map = if kind == "sub" {
        subscripts()
    } else {
        superscripts()
    };
    if value.len() == 1 {
        let char = value.chars().next().unwrap();
        if let Some(mapped) = symbol_lookup(map, &char.to_string()) {
            return mapped.to_string();
        }
    }
    if kind == "sub" {
        format!("_{{{value}}}")
    } else {
        format!("^{{{value}}}")
    }
}

fn format_fraction(numerator: &str, denominator: &str) -> String {
    format!("({numerator}/{denominator})")
}

fn format_root(value: &str, symbol: &str) -> String {
    format!("{symbol}({value})")
}

// ---------------------------------------------------------------------------
// Parser
// ---------------------------------------------------------------------------

pub struct LatexParser {
    source: Vec<char>,
    layout_nodes: Vec<LayoutNode>,
    display: bool,
    position: usize,
    supported: bool,
    stack_fractions: bool,
}

impl LatexParser {
    pub fn new(source: &str, layout_nodes: Vec<LayoutNode>, display: bool) -> Self {
        Self {
            source: source.chars().collect(),
            layout_nodes,
            display,
            position: 0,
            supported: true,
            stack_fractions: true,
        }
    }

    pub fn render(&mut self) -> Option<String> {
        let rendered = self.parse_sequence(None);
        if !self.supported || self.position != self.source.len() {
            return None;
        }
        Some(normalize_output(&rendered))
    }

    fn parse_sequence(&mut self, end_character: Option<char>) -> String {
        let mut result = String::new();
        while self.position < self.source.len() {
            let character = self.source[self.position];
            if let Some(end_character) = end_character {
                if character == end_character {
                    self.position += 1;
                    return result;
                }
            }
            if character == '}' {
                self.supported = false;
                return result;
            }
            if character == '{' {
                self.position += 1;
                result += &self.parse_sequence(Some('}'));
                continue;
            }
            if character == '\\' {
                let command = self.parse_command();
                if command == NEGATIVE_SPACE.to_string() {
                    result = result.trim_end().to_string();
                    if result.ends_with(NAMED_OPERATOR_END) {
                        result.truncate(result.len() - NAMED_OPERATOR_END.len_utf8());
                    }
                } else {
                    result += &command;
                }
                continue;
            }
            if character == '^' || character == '_' {
                self.position += 1;
                result = result.trim_end().to_string();
                let argument = self.parse_required_argument(false);
                let script = format_script(&argument, if character == '_' { "sub" } else { "sup" });
                if result.ends_with(NAMED_OPERATOR_END) {
                    result.truncate(result.len() - NAMED_OPERATOR_END.len_utf8());
                    result += &script;
                    result.push(NAMED_OPERATOR_END);
                } else {
                    result += &script;
                }
                continue;
            }
            if character.is_whitespace() {
                result += &self.parse_whitespace();
                continue;
            }
            if character == '=' || character == '<' || character == '>' {
                result = format!("{} {character} ", result.trim_end());
                self.position += 1;
                continue;
            }
            if character == '&' {
                self.position += 1;
                continue;
            }
            if character == '~' {
                self.position += 1;
                result.push(' ');
                continue;
            }
            if character == '.' {
                // Trailing layout marker handling for matrix rows.
                if let Some(node_index) = self.trailing_layout_marker_index(&result) {
                    if let Some(LayoutNode::Matrix { lines, .. }) = self.layout_nodes.get_mut(node_index) {
                        let last_line = lines.len() - 1;
                        lines[last_line].push('.');
                        self.position += 1;
                        continue;
                    }
                }
            }
            result.push(character);
            self.position += 1;
        }
        if end_character.is_some() {
            self.supported = false;
        }
        result
    }

    fn trailing_layout_marker_index(&self, result: &str) -> Option<usize> {
        // Regex: /\u{f0000}(\d+)\u{f0001}$/u
        let chars: Vec<char> = result.chars().collect();
        if chars.len() < 3 {
            return None;
        }
        if chars[0] != LAYOUT_MARKER_START || *chars.last()? != LAYOUT_MARKER_END {
            return None;
        }
        let digits: String = chars[1..chars.len() - 1].iter().collect();
        digits.parse::<usize>().ok()
    }

    fn parse_whitespace(&mut self) -> String {
        while self.position < self.source.len() && self.source[self.position].is_whitespace() {
            self.position += 1;
        }
        " ".to_string()
    }

    fn parse_command(&mut self) -> String {
        self.position += 1;
        if self.position >= self.source.len() {
            self.supported = false;
            return String::new();
        }
        let command;
        let first = self.source[self.position];
        if first.is_ascii_alphabetic() {
            let start = self.position;
            while self.position < self.source.len() && self.source[self.position].is_ascii_alphabetic() {
                self.position += 1;
            }
            command = self.source[start..self.position].iter().collect();
        } else {
            command = first.to_string();
            self.position += 1;
        }

        if command == "\\" {
            return "\n".to_string();
        }
        if set_contains(spacing_commands(), &command) {
            return " ".to_string();
        }
        if set_contains(negative_spacing_commands(), &command) {
            return NEGATIVE_SPACE.to_string();
        }
        if set_contains(ignored_commands(), &command) {
            return String::new();
        }
        if matches!(command.as_str(), "{" | "}" | "$" | "%" | "#" | "_" | "&") {
            return command;
        }
        if command == "|" {
            return "‖".to_string();
        }
        if command == "not" {
            let value = self.parse_required_argument(false).trim().to_string();
            if let Some(negated) = symbol_lookup(negated_symbols(), &value) {
                return format!(" {negated} ");
            }
            let characters: Vec<char> = value.chars().collect();
            if characters.is_empty() {
                self.supported = false;
                return String::new();
            }
            let rest: String = characters[1..].iter().collect();
            return format!(" {}\u{0338}{rest} ", characters[0]);
        }
        if set_contains(limit_operators(), &command) {
            return self.parse_operator(&command, "bracket", true, true);
        }

        if let Some(symbol) = symbol_lookup(latex_symbols(), &command) {
            if set_contains(display_limit_symbols(), &command) {
                return self.parse_operator(symbol, "script", true, false);
            }
            return if command == "cdot" || command == "times" || set_contains(relation_commands(), &command) {
                format!(" {symbol} ")
            } else {
                symbol.to_string()
            };
        }
        if set_contains(named_operators(), &command) {
            return format!("{NAMED_OPERATOR_START}{command}{NAMED_OPERATOR_END}");
        }
        if set_contains(size_commands(), &command) {
            return String::new();
        }
        if matches!(command.as_str(), "left" | "middle" | "right") {
            if self.source.get(self.position) == Some(&'.') {
                self.position += 1;
            }
            return String::new();
        }
        if matches!(command.as_str(), "frac" | "dfrac" | "tfrac") {
            let should_stack = self.display && self.stack_fractions && command != "tfrac";
            let numerator = self.parse_required_argument(!should_stack);
            let denominator = self.parse_required_argument(!should_stack);
            if should_stack {
                self.layout_nodes.push(LayoutNode::Fraction {
                    numerator: normalize_output(&numerator),
                    denominator: normalize_output(&denominator),
                });
                let index = self.layout_nodes.len() - 1;
                return format!("{LAYOUT_MARKER_START}{index}{LAYOUT_MARKER_END}");
            }
            return format_fraction(&numerator, &denominator);
        }
        if command == "sqrt" {
            let degree = self.parse_optional_argument();
            let value = self.parse_required_argument(true);
            match degree.as_deref().map(|d| d.trim()) {
                None | Some("2") => format_root(&value, "√"),
                Some("3") => format_root(&value, "∛"),
                Some("4") => format_root(&value, "∜"),
                Some(degree) => format!("{}{}", format_script(degree, "sup"), format_root(&value, "√")),
            }
        } else if command == "boxed" || command == "fbox" {
            format!("[{}]", self.parse_required_argument(true).trim())
        } else if matches!(command.as_str(), "binom" | "dbinom" | "tbinom") {
            format!(
                "({} choose {})",
                self.parse_required_argument(true),
                self.parse_required_argument(true)
            )
        } else if let Some(accent) = symbol_lookup(accents(), &command) {
            let value = self.parse_required_argument(true);
            if value.chars().count() == 1 {
                format!("{value}{accent}")
            } else {
                format!("{command}({value})")
            }
        } else if command == "mathbb" {
            let value = self.parse_required_argument(true);
            value
                .chars()
                .map(|char| {
                    blackboard()
                        .iter()
                        .find(|(k, _)| *k == char)
                        .map(|(_, v)| *v)
                        .unwrap_or(char)
                })
                .collect()
        } else if command == "operatorname" {
            if self.source.get(self.position) == Some(&'*') {
                self.position += 1;
            }
            let operator = normalize_output(&self.parse_required_argument(true)).trim().to_string();
            self.parse_operator(&operator, "bracket", true, true)
        } else if command == "mod" || command == "bmod" {
            " mod ".to_string()
        } else if command == "pmod" || command == "pod" {
            let value = self.parse_required_argument(true).trim().to_string();
            if command == "pmod" {
                format!(" (mod {value})")
            } else {
                format!(" ({value})")
            }
        } else if command == "overset" || command == "stackrel" {
            let upper = self.parse_required_argument(true);
            let value = self.parse_required_argument(true).trim().to_string();
            format!("{value}{}", format_script(&upper, "sup"))
        } else if command == "underset" {
            let lower = self.parse_required_argument(true);
            let value = self.parse_required_argument(true).trim().to_string();
            format!("{value}{}", format_script(&lower, "sub"))
        } else if set_contains(plain_wrappers(), &command) {
            let value = self.parse_required_argument(true);
            if command.starts_with("text") || command == "mbox" {
                value
            } else {
                value.trim().to_string()
            }
        } else if command == "begin" {
            self.parse_environment()
        } else if command == "end" {
            self.supported = false;
            String::new()
        } else {
            self.supported = false;
            format!("\\{command}")
        }
    }

    fn parse_operator(&mut self, operator: &str, inline_lower_style: &str, display_limits: bool, spaced: bool) -> String {
        let mut use_display_limits = display_limits;
        let mut modifier_position = self.position;
        while modifier_position < self.source.len()
            && (self.source[modifier_position] == ' ' || self.source[modifier_position] == '\t')
        {
            modifier_position += 1;
        }
        let modifier: String = self.source[modifier_position..].iter().collect();
        if let Some(_rest) = modifier.strip_prefix("\\limits") {
            use_display_limits = true;
            self.position = modifier_position + "\\limits".len();
        } else if modifier.strip_prefix("\\nolimits").is_some() {
            use_display_limits = false;
            self.position = modifier_position + "\\nolimits".len();
        }

        let mut lower: Option<String> = None;
        let mut upper: Option<String> = None;
        loop {
            let mut script_position = self.position;
            while script_position < self.source.len()
                && (self.source[script_position] == ' ' || self.source[script_position] == '\t')
            {
                script_position += 1;
            }
            let kind = self.source.get(script_position).copied();
            if kind != Some('_') && kind != Some('^') {
                break;
            }
            self.position = script_position + 1;
            let value = normalize_output(&self.parse_required_argument(false)).replace(' ', "");
            if kind == Some('_') {
                if lower.is_some() {
                    self.supported = false;
                }
                lower = Some(value);
            } else {
                if upper.is_some() {
                    self.supported = false;
                }
                upper = Some(value);
            }
        }

        if self.display && use_display_limits && (lower.is_some() || upper.is_some()) {
            self.layout_nodes.push(LayoutNode::Operator {
                operator: operator.to_string(),
                lower: lower.clone(),
                upper: upper.clone(),
            });
            let index = self.layout_nodes.len() - 1;
            return format!("{LAYOUT_MARKER_START}{index}{LAYOUT_MARKER_END}");
        }

        let mut rendered = operator.to_string();
        if let Some(lower) = &lower {
            rendered += &if inline_lower_style == "bracket" {
                format!("[{lower}]")
            } else {
                format_script(lower, "sub")
            };
        }
        if let Some(upper) = &upper {
            rendered += &format_script(upper, "sup");
        }
        if spaced {
            format!(" {rendered} ")
        } else {
            rendered
        }
    }

    fn parse_required_argument(&mut self, stack_fractions: bool) -> String {
        let previous_stack_fractions = self.stack_fractions;
        self.stack_fractions = previous_stack_fractions && stack_fractions;
        let value = self.parse_required_argument_value();
        self.stack_fractions = previous_stack_fractions;
        value
    }

    fn parse_required_argument_value(&mut self) -> String {
        while self.position < self.source.len()
            && (self.source[self.position] == ' ' || self.source[self.position] == '\t')
        {
            self.position += 1;
        }
        if self.position >= self.source.len() {
            self.supported = false;
            return String::new();
        }
        if self.source[self.position] == '{' {
            self.position += 1;
            return self.parse_sequence(Some('}'));
        }
        if self.source[self.position] == '\\' {
            return self.parse_command();
        }
        let value = self.source[self.position].to_string();
        self.position += 1;
        value
    }

    fn parse_optional_argument(&mut self) -> Option<String> {
        while self.position < self.source.len()
            && (self.source[self.position] == ' ' || self.source[self.position] == '\t')
        {
            self.position += 1;
        }
        if self.source.get(self.position) != Some(&'[') {
            return None;
        }
        let start = self.position + 1;
        let end = self.source[start..].iter().position(|c| *c == ']');
        let Some(end) = end else {
            self.supported = false;
            return None;
        };
        let value: String = self.source[start..start + end].iter().collect();
        self.position = start + end + 1;
        Some(self.render_nested(&value, true))
    }

    fn read_raw_group(&mut self) -> Option<String> {
        while self.position < self.source.len()
            && (self.source[self.position] == ' ' || self.source[self.position] == '\t')
        {
            self.position += 1;
        }
        if self.source.get(self.position) != Some(&'{') {
            self.supported = false;
            return None;
        }
        let start = self.position + 1;
        let mut depth = 1usize;
        let mut cursor = self.position + 1;
        while cursor < self.source.len() {
            let character = self.source[cursor];
            if character == '\\' {
                cursor += 2;
                continue;
            }
            if character == '{' {
                depth += 1;
            }
            if character == '}' {
                depth -= 1;
            }
            if depth == 0 {
                let value: String = self.source[start..cursor].iter().collect();
                self.position = cursor + 1;
                return Some(value);
            }
            cursor += 1;
        }
        self.supported = false;
        None
    }

    fn split_environment_rows(&self, body: &str) -> Vec<String> {
        // Split on \\ optionally followed by [..]
        let mut rows: Vec<String> = Vec::new();
        let mut rest = body;
        while let Some(index) = rest.find("\\\\") {
            rows.push(rest[..index].to_string());
            let after = &rest[index + 2..];
            if let Some(opt_end) = after.find(']') {
                if after.starts_with('[') {
                    rest = &after[opt_end + 1..];
                    continue;
                }
            }
            rest = after;
        }
        rows.push(rest.to_string());
        rows
    }

    fn parse_environment(&mut self) -> String {
        let Some(environment) = self.read_raw_group() else {
            return String::new();
        };
        let end_marker = format!("\\end{{{environment}}}");
        let body_end: String = self.source[self.position..].iter().collect();
        let Some(end) = body_end.find(&end_marker) else {
            self.supported = false;
            return String::new();
        };
        let body: String = self.source[self.position..self.position + end].iter().collect();
        self.position += end + end_marker.len();

        if matches!(environment.as_str(), "equation" | "equation*" | "displaymath") {
            return self.render_nested(&body, true).trim().to_string();
        }

        if matches!(
            environment.as_str(),
            "aligned"
                | "align"
                | "align*"
                | "alignedat"
                | "alignat"
                | "alignat*"
                | "gather"
                | "gathered"
                | "multline"
                | "multline*"
                | "split"
        ) {
            let aligned_at = matches!(environment.as_str(), "alignedat" | "alignat" | "alignat*");
            let aligned_body = if aligned_at {
                body.replacen('{', "", 1)
            } else {
                body.clone()
            };
            let aligned_body = aligned_body.trim_start().to_string();
            let rows = self.split_environment_rows(&aligned_body);
            let rendered_rows: Vec<String> = rows
                .iter()
                .map(|row| {
                    let cells: Vec<&str> = row.split('&').collect();
                    let source = if aligned_at {
                        (0..(cells.len() + 1) / 2)
                            .map(|index| {
                                let start = index * 2;
                                let end = (start + 2).min(cells.len());
                                cells[start..end].join("")
                            })
                            .collect::<Vec<_>>()
                            .join(" ")
                    } else {
                        cells.join("")
                    };
                    self.render_nested(&source, true).trim().to_string()
                })
                .filter(|row| !row.is_empty())
                .collect();
            return rendered_rows.join("\n");
        }

        if environment == "cases" || environment == "cases*" {
            let rows: Vec<Vec<String>> = self
                .split_environment_rows(&body)
                .iter()
                .map(|row| {
                    row.split('&')
                        .map(|cell| self.render_nested(cell, false).trim().to_string())
                        .collect()
                })
                .filter(|row: &Vec<String>| row.iter().any(|cell| !cell.is_empty()))
                .collect();
            let mut rendered_rows: Vec<String> = Vec::new();
            for (index, row) in rows.iter().enumerate() {
                let value = row.first().cloned().unwrap_or_default();
                let value = value.trim_end_matches(',').to_string();
                let condition = row.get(1).cloned().unwrap_or_default();
                let delimiter = if index == 0 {
                    "⎧"
                } else if index == rows.len() - 1 {
                    "⎩"
                } else {
                    "⎨"
                };
                let condition_prefix = if condition.is_empty() {
                    ""
                } else if regex_condition(&condition) {
                    " "
                } else {
                    " if "
                };
                rendered_rows.push(format!(
                    "{delimiter} {value}{}{condition}",
                    condition_prefix
                ));
            }
            return rendered_rows.join("\n");
        }

        if matches!(
            environment.as_str(),
            "array" | "matrix" | "smallmatrix" | "pmatrix" | "bmatrix" | "Bmatrix" | "vmatrix" | "Vmatrix"
        ) {
            let matrix_body = if environment == "array" {
                body.replacen('{', "", 1)
            } else {
                body.clone()
            };
            return self.render_matrix(&environment, &matrix_body);
        }

        self.supported = false;
        body
    }

    fn render_matrix(&mut self, environment: &str, body: &str) -> String {
        let matrix: Vec<Vec<String>> = self
            .split_environment_rows(body)
            .iter()
            .map(|row| {
                row.split('&')
                    .map(|cell| self.render_nested(cell, false).trim().to_string())
                    .collect()
            })
            .filter(|row: &Vec<String>| row.iter().any(|cell| !cell.is_empty()))
            .collect();
        let column_count = matrix
            .iter()
            .map(|row| row.len())
            .fold(0usize, usize::max);
        let mut column_widths: Vec<f64> = Vec::new();
        for column in 0..column_count {
            let width = matrix
                .iter()
                .map(|row| {
                    row.get(column)
                        .map(|cell| visible_width_of(cell))
                        .unwrap_or(0.0)
                })
                .fold(0.0, f64::max);
            column_widths.push(width);
        }
        let rows: Vec<String> = matrix
            .iter()
            .map(|row| {
                let cells: Vec<String> = (0..column_count)
                    .map(|column| {
                        let cell = row.get(column).cloned().unwrap_or_default();
                        let pad = ((column_widths[column] - visible_width_of(&cell)).max(0.0)) as usize;
                        format!("{cell}{}", PROTECTED_SPACE.to_string().repeat(pad))
                    })
                    .collect();
                cells.join(" │ ")
            })
            .collect();

        let lines: Vec<String> = if matches!(environment, "array" | "matrix" | "smallmatrix") {
            rows.clone()
        } else {
            let delimiters: Option<(&str, &str, &str, &str, &str, &str)> = match environment {
                "pmatrix" => Some(("⎛", "⎞", "⎜", "⎟", "⎝", "⎠")),
                "bmatrix" => Some(("⎡", "⎤", "⎢", "⎥", "⎣", "⎦")),
                "Bmatrix" => Some(("⎧", "⎫", "⎨", "⎬", "⎩", "⎭")),
                "vmatrix" => Some(("│", "│", "│", "│", "│", "│")),
                "Vmatrix" => Some(("║", "║", "║", "║", "║", "║")),
                _ => None,
            };
            match delimiters {
                None => {
                    self.supported = false;
                    return rows.join("\n");
                }
                Some((top_left, top_right, mid_left, mid_right, bottom_left, bottom_right)) => {
                    rows.iter()
                        .enumerate()
                        .map(|(index, row)| {
                            let (left, right) = if index == 0 {
                                (top_left, top_right)
                            } else if index == rows.len() - 1 {
                                (bottom_left, bottom_right)
                            } else {
                                (mid_left, mid_right)
                            };
                            format!("{left} {row} {right}")
                        })
                        .collect()
                }
            }
        };

        if lines.len() <= 1 {
            return lines.first().cloned().unwrap_or_default();
        }
        self.layout_nodes.push(LayoutNode::Matrix {
            lines,
            baseline: 0.0,
        });
        let index = self.layout_nodes.len() - 1;
        format!("{LAYOUT_MARKER_START}{index}{LAYOUT_MARKER_END}")
    }

    fn render_nested(&mut self, source: &str, stack_fractions: bool) -> String {
        let nodes = std::mem::take(&mut self.layout_nodes);
        let mut parser = LatexParser::new(source, nodes, self.display && stack_fractions);
        let rendered = parser.render();
        self.layout_nodes = parser.layout_nodes;
        match rendered {
            Some(rendered) => rendered,
            None => {
                self.supported = false;
                source.to_string()
            }
        }
    }
}

fn regex_condition(condition: &str) -> bool {
    let lower = condition.to_lowercase();
    lower.starts_with("if ")
        || lower.starts_with("when ")
        || lower.starts_with("for ")
        || lower.starts_with("otherwise")
}

/// Normalize parser output: named-operator spacing, line whitespace cleanup.
pub fn normalize_output(value: &str) -> String {
    let mut result = value.to_string();
    // Replace \u{f0004} preceded by letters/numbers/closing brackets with space.
    let chars: Vec<char> = result.chars().collect();
    let mut out = String::new();
    for (index, char) in chars.iter().enumerate() {
        if *char == NAMED_OPERATOR_START {
            if index > 0 {
                let prev = chars[index - 1];
                if prev.is_alphanumeric() || matches!(prev, ')' | '}' | '\u{f0001}') {
                    out.push(' ');
                }
            }
            continue;
        }
        out.push(*char);
    }
    result = out;
    result = result.replace(NAMED_OPERATOR_END, "");
    let chars: Vec<char> = result.chars().collect();
    let mut out = String::new();
    for (index, char) in chars.iter().enumerate() {
        if *char == NAMED_OPERATOR_END {
            if index + 1 < chars.len() {
                let next = chars[index + 1];
                if next.is_alphanumeric() || matches!(next, '√' | '\u{f0000}') {
                    out.push(' ');
                }
            }
            continue;
        }
        out.push(*char);
    }
    result = out;
    result = result
        .split('\n')
        .map(|line| line.split(|c: char| c == ' ' || c == '\t').filter(|s| !s.is_empty()).collect::<Vec<_>>().join(" ").trim().to_string())
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    result.trim().to_string()
}

#[derive(Clone, Debug, PartialEq)]
pub struct RenderLatexOptions {
    pub display: bool,
}

impl Default for RenderLatexOptions {
    fn default() -> Self {
        Self { display: false }
    }
}

/// Render LaTeX source to terminal text.
pub fn render_latex(source: &str, options: &RenderLatexOptions) -> Option<String> {
    let layout_nodes: Vec<LayoutNode> = Vec::new();
    let mut parser = LatexParser::new(source, layout_nodes, options.display);
    let rendered = parser.render()?;
    let layout_nodes = parser.layout_nodes;
    if layout_nodes.is_empty() {
        return Some(rendered.replace(PROTECTED_SPACE, " "));
    }
    let layout = render_layout(&rendered, &layout_nodes);
    let non_empty: Vec<&String> = layout.lines.iter().filter(|line| !line.trim().is_empty()).collect();
    let indentation = non_empty
        .iter()
        .map(|line| line.len() - line.trim_start().len())
        .min()
        .unwrap_or(0);
    let joined = layout
        .lines
        .iter()
        .map(|line| {
            let stripped = line.strip_prefix(&" ".repeat(indentation)).unwrap_or(line);
            stripped.trim_end().to_string()
        })
        .collect::<Vec<_>>()
        .join("\n")
        .trim_end()
        .to_string();
    Some(joined.replace(PROTECTED_SPACE, " "))
}

// ---------------------------------------------------------------------------
// Symbol tables (generated from latex.ts)
// ---------------------------------------------------------------------------

include!("latex_tables.rs");

#[cfg(test)]
mod tests {
    use super::*;

    fn render(source: &str) -> String {
        render_latex(source, &RenderLatexOptions::default()).unwrap()
    }

    fn render_display(source: &str) -> String {
        render_latex(source, &RenderLatexOptions { display: true }).unwrap()
    }

    #[test]
    fn renders_greek_letters() {
        assert_eq!(render("\\alpha + \\beta"), "α + β");
        assert_eq!(render("\\Gamma"), "Γ");
    }

    #[test]
    fn renders_operators_with_spacing() {
        assert_eq!(render("a \\times b"), "a × b");
        assert_eq!(render("x \\in S"), "x ∈ S");
        assert_eq!(render("a \\leq b"), "a ≤ b");
    }

    #[test]
    fn renders_fractions_inline() {
        assert_eq!(render("\\frac{a}{b}"), "(a/b)");
        assert_eq!(render("\\frac{1}{2} + \\frac{3}{4}"), "(1/2) + (3/4)");
    }

    #[test]
    fn renders_superscripts_and_subscripts() {
        assert_eq!(render("x^2"), "x²");
        assert_eq!(render("x_i"), "xᵢ");
        assert_eq!(render("a^2 + b^2 = c^2"), "a² + b² = c²");
    }

    #[test]
    fn renders_sqrt() {
        assert_eq!(render("\\sqrt{x}"), "√(x)");
        assert_eq!(render("\\sqrt[3]{x}"), "∛(x)");
    }

    #[test]
    fn renders_sum_with_limits_inline() {
        // JS formatScript: multi-char scripts keep the _{...} form and
        // operator limit values have spaces stripped.
        assert_eq!(render("\\sum_{i=1}^{n} i"), "∑_{i=1}ⁿ i");
    }

    #[test]
    fn renders_named_operators() {
        assert_eq!(render("\\sin x"), "sin x");
        // Operator limit values have spaces stripped (JS replaceAll).
        assert_eq!(render("\\lim_{x \\to 0} f(x)"), "lim[x→0] f(x)");
    }

    #[test]
    fn display_stacks_fractions() {
        let rendered = render_display("\\frac{a}{b}");
        assert!(rendered.contains('─'));
        assert!(rendered.contains('a'));
        assert!(rendered.contains('b'));
    }

    #[test]
    fn display_stacks_operator_limits() {
        let rendered = render_display("\\sum_{i=1}^{n}");
        assert!(rendered.contains('∑'));
        assert!(rendered.contains("i=1"));
        assert!(rendered.contains('n'));
    }

    #[test]
    fn renders_matrices() {
        let rendered = render("\\begin{pmatrix} a & b \\\\ c & d \\end{pmatrix}");
        assert!(rendered.contains('a'));
        assert!(rendered.contains('b'));
        assert!(rendered.contains('c'));
        assert!(rendered.contains('d'));
    }

    #[test]
    fn renders_align_environment() {
        let rendered = render("\\begin{align} a &= b \\\\ c &= d \\end{align}");
        assert!(rendered.contains('a'));
        assert!(rendered.contains('c'));
    }

    #[test]
    fn unsupported_returns_none() {
        assert_eq!(render_latex("\\unknowncommand", &RenderLatexOptions::default()), None);
        assert_eq!(render_latex("\\begin{unknown}", &RenderLatexOptions::default()), None);
    }

    #[test]
    fn plain_text_passes_through() {
        assert_eq!(render("hello world"), "hello world");
        assert_eq!(render(""), "");
    }

    #[test]
    fn script_maps() {
        assert!(superscripts().iter().any(|(k, _)| *k == "2"));
        assert!(subscripts().iter().any(|(k, _)| *k == "i"));
        assert!(latex_symbols().iter().any(|(k, _)| *k == "alpha"));
    }
}
