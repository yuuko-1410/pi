//! Markdown component, port of `packages/tui/src/components/markdown.ts`.
//!
//! Difference: the `marked` parser is replaced by a hand-written lexer
//! covering the common block forms (headings, fenced code, lists with
//! nesting, blockquotes, horizontal rules, tables, paragraphs) and inline
//! forms (strong, em, codespan, links, strikethrough, inline latex, br).

use std::sync::Arc;

use crate::latex::render_latex;
use crate::terminal_image::{get_capabilities, is_image_line};
use crate::tui::Component;
use crate::utils::{visible_width, wrap_text_with_ansi};
use crate::components::basic::apply_background_to_line;

// ---------------------------------------------------------------------------
// Inline tokenizer
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
pub enum InlineToken {
    Text(String),
    Strong(String),
    Em(String),
    Codespan(String),
    Link { text: String, href: String },
    Del(String),
    Br,
    Latex { text: String, raw: String },
    Escape(String),
}

/// Tokenize inline markdown into tokens (marked-compatible subset).
pub fn tokenize_inline(source: &str) -> Vec<InlineToken> {
    let mut tokens: Vec<InlineToken> = Vec::new();
    let chars: Vec<char> = source.chars().collect();
    let mut i = 0usize;
    let mut text = String::new();

    let flush_text = |text: &mut String, tokens: &mut Vec<InlineToken>| {
        if !text.is_empty() {
            tokens.push(InlineToken::Text(std::mem::take(text)));
        }
    };

    while i < chars.len() {
        let c = chars[i];
        // Inline latex: $...$
        if c == '$' {
            if let Some(end) = chars[i + 1..].iter().position(|c| *c == '$') {
                let latex_text: String = chars[i + 1..i + 1 + end].iter().collect();
                flush_text(&mut text, &mut tokens);
                tokens.push(InlineToken::Latex {
                    text: latex_text.clone(),
                    raw: format!("${latex_text}$"),
                });
                i += end + 2;
                continue;
            }
        }
        // Strikethrough ~~text~~
        if c == '~' && chars.get(i + 1) == Some(&'~') {
            if let Some(end) = chars[i + 2..].iter().position(|c| *c == '~') {
                let content: String = chars[i + 2..i + 2 + end].iter().collect();
                flush_text(&mut text, &mut tokens);
                tokens.push(InlineToken::Del(tokenize_inline(&content).into_string()));
                i += end + 4;
                continue;
            }
        }
        // Strong **text**
        if c == '*' && chars.get(i + 1) == Some(&'*') {
            let closing = chars[i + 2..]
                .iter()
                .position(|c| *c == '*')
                .map(|index| i + 2 + index);
            if let Some(closing) = closing {
                if chars.get(closing + 1) == Some(&'*') {
                    let content: String = chars[i + 2..closing].iter().collect();
                    flush_text(&mut text, &mut tokens);
                    tokens.push(InlineToken::Strong(tokenize_inline(&content).into_string()));
                    i = closing + 2;
                    continue;
                }
            }
        }
        // Em *text*
        if c == '*' {
            if let Some(end) = chars[i + 1..].iter().position(|c| *c == '*') {
                let content: String = chars[i + 1..i + 1 + end].iter().collect();
                flush_text(&mut text, &mut tokens);
                tokens.push(InlineToken::Em(tokenize_inline(&content).into_string()));
                i += end + 2;
                continue;
            }
        }
        // Codespan `text`
        if c == '`' {
            if let Some(end) = chars[i + 1..].iter().position(|c| *c == '`') {
                let content: String = chars[i + 1..i + 1 + end].iter().collect();
                flush_text(&mut text, &mut tokens);
                tokens.push(InlineToken::Codespan(content));
                i += end + 2;
                continue;
            }
        }
        // Link [text](href)
        if c == '[' {
            if let Some(close) = chars[i + 1..].iter().position(|c| *c == ']') {
                let text_content: String = chars[i + 1..i + 1 + close].iter().collect();
                if chars.get(i + close + 2) == Some(&'(') {
                    if let Some(href_end) = chars[i + close + 3..].iter().position(|c| *c == ')') {
                        let href: String = chars[i + close + 3..i + close + 3 + href_end].iter().collect();
                        flush_text(&mut text, &mut tokens);
                        tokens.push(InlineToken::Link {
                            text: tokenize_inline(&text_content).into_string(),
                            href,
                        });
                        i += close + 4 + href_end;
                        continue;
                    }
                }
            }
        }
        // Escape \x
        if c == '\\' && i + 1 < chars.len() {
            text.push(chars[i + 1]);
            i += 2;
            continue;
        }
        // Hard break: two spaces + newline
        if c == '\n' {
            if text.ends_with("  ") {
                text.truncate(text.len() - 2);
                flush_text(&mut text, &mut tokens);
                tokens.push(InlineToken::Br);
            } else {
                text.push('\n');
            }
            i += 1;
            continue;
        }
        text.push(c);
        i += 1;
    }
    flush_text(&mut text, &mut tokens);
    tokens
}

trait IntoString {
    fn into_string(self) -> String;
}

impl IntoString for Vec<InlineToken> {
    fn into_string(self) -> String {
        self.into_iter()
            .map(|token| match token {
                InlineToken::Text(text)
                | InlineToken::Strong(text)
                | InlineToken::Em(text)
                | InlineToken::Codespan(text)
                | InlineToken::Del(text)
                | InlineToken::Escape(text) => text,
                InlineToken::Link { text, .. } => text,
                InlineToken::Br => "\n".to_string(),
                InlineToken::Latex { raw, .. } => raw,
            })
            .collect()
    }
}

// ---------------------------------------------------------------------------
// Block tokenizer
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
pub enum BlockToken {
    Heading { depth: usize, inline: Vec<InlineToken> },
    Paragraph(Vec<InlineToken>),
    Text(Vec<InlineToken>),
    Code { lang: String, text: String },
    List {
        ordered: bool,
        start: f64,
        loose: bool,
        items: Vec<ListItem>,
    },
    Table {
        header: Vec<Vec<InlineToken>>,
        rows: Vec<Vec<Vec<InlineToken>>>,
        raw: String,
    },
    Blockquote(Vec<BlockToken>),
    Hr,
    Html(String),
    Space,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ListItem {
    pub raw: String,
    pub task: bool,
    pub checked: bool,
    pub tokens: Vec<BlockToken>,
}

/// Tokenize markdown into block tokens (marked-compatible subset).
pub fn tokenize_blocks(source: &str) -> Vec<BlockToken> {
    let mut tokens: Vec<BlockToken> = Vec::new();
    let lines: Vec<&str> = source.split('\n').collect();
    let mut i = 0usize;

    while i < lines.len() {
        let line = lines[i];

        // Blank line.
        if line.trim().is_empty() {
            // Collapse consecutive blank lines into one space token.
            if !matches!(tokens.last(), Some(BlockToken::Space)) {
                tokens.push(BlockToken::Space);
            }
            i += 1;
            continue;
        }

        // Heading: # ... (up to 6)
        let leading = line.chars().take_while(|c| *c == '#').count();
        if leading > 0 && leading <= 6 {
            let rest = &line[leading..];
            if rest.starts_with(' ') {
                tokens.push(BlockToken::Heading {
                    depth: leading,
                    inline: tokenize_inline(rest.trim()),
                });
                i += 1;
                continue;
            }
        }

        // Fenced code: ```lang
        if line.starts_with("```") {
            let lang = line[3..].trim().to_string();
            let mut code_lines: Vec<String> = Vec::new();
            i += 1;
            while i < lines.len() && !lines[i].starts_with("```") {
                code_lines.push(lines[i].to_string());
                i += 1;
            }
            i += 1; // skip closing fence
            tokens.push(BlockToken::Code {
                lang,
                text: code_lines.join("\n"),
            });
            continue;
        }

        // Horizontal rule: ---
        if is_hr(line) {
            tokens.push(BlockToken::Hr);
            i += 1;
            continue;
        }

        // Blockquote: > ...
        if line.starts_with('>') {
            let mut quote_lines: Vec<String> = Vec::new();
            while i < lines.len() {
                let quote_line = lines[i];
                if let Some(rest) = quote_line.strip_prefix('>') {
                    quote_lines.push(rest.strip_prefix(' ').unwrap_or(rest).to_string());
                    i += 1;
                } else if quote_line.trim().is_empty() {
                    // Blockquote continues over blank lines if the next is a quote.
                    if lines.get(i + 1).map(|next| next.starts_with('>')).unwrap_or(false) {
                        quote_lines.push(String::new());
                        i += 1;
                    } else {
                        break;
                    }
                } else {
                    break;
                }
            }
            tokens.push(BlockToken::Blockquote(tokenize_blocks(&quote_lines.join("\n"))));
            continue;
        }

        // Table: header | ... then separator |---|
        if line.contains('|') && lines.get(i + 1).map(|next| is_table_separator(next)).unwrap_or(false) {
            let header: Vec<Vec<InlineToken>> = split_table_row(line)
                .into_iter()
                .map(|cell| tokenize_inline(cell.trim()))
                .collect();
            let mut rows: Vec<Vec<Vec<InlineToken>>> = Vec::new();
            i += 2;
            while i < lines.len() && lines[i].contains('|') && !lines[i].trim().is_empty() {
                let row: Vec<Vec<InlineToken>> = split_table_row(lines[i])
                    .into_iter()
                    .map(|cell| tokenize_inline(cell.trim()))
                    .collect();
                rows.push(row);
                i += 1;
            }
            tokens.push(BlockToken::Table {
                header,
                rows,
                raw: String::new(),
            });
            continue;
        }

        // List.
        if let Some((ordered, start, marker_len)) = list_marker(line) {
            let mut items: Vec<ListItem> = Vec::new();
            let mut loose = false;
            let mut collected_lines: Vec<(String, usize)> = Vec::new(); // (content, indent)
            let mut current_indent = marker_len;
            // The first list line starts the first item; subsequent marker
            // lines flush the current item. The marker already consumed the
            // trailing space, so the rest is the item content (a leading
            // space may remain for indented markers).
            {
                let mut content: String = line.chars().skip(marker_len).collect();
                if content.starts_with(' ') {
                    content.remove(0);
                }
                collected_lines.push((content, marker_len));
            }
            i += 1;
            while i < lines.len() {
                let list_line = lines[i];
                if let Some((_, _, next_marker_len)) = list_marker(list_line) {
                    if next_marker_len <= current_indent + 2 {
                        // Flush current item.
                        items.push(build_list_item(&collected_lines, ordered, current_indent));
                        collected_lines.clear();
                        current_indent = next_marker_len;
                        let mut content: String = list_line.chars().skip(next_marker_len).collect();
                        if content.starts_with(' ') {
                            content.remove(0);
                        }
                        collected_lines.push((content, next_marker_len));
                        i += 1;
                        continue;
                    }
                }
                if list_line.trim().is_empty() {
                    if lines.get(i + 1).map(|next| list_marker(next).is_some()).unwrap_or(false) {
                        loose = true;
                        i += 1;
                        continue;
                    }
                    // Continuation blank inside an item.
                    collected_lines.push((String::new(), current_indent));
                    i += 1;
                    continue;
                }
                collected_lines.push((list_line.to_string(), current_indent));
                i += 1;
            }
            if !collected_lines.is_empty() {
                items.push(build_list_item(&collected_lines, ordered, current_indent));
            }
            tokens.push(BlockToken::List {
                ordered,
                start,
                loose,
                items,
            });
            continue;
        }

        // Paragraph: gather until blank line or block start.
        let mut paragraph_lines: Vec<String> = vec![line.to_string()];
        i += 1;
        while i < lines.len() {
            let next = lines[i];
            if next.trim().is_empty()
                || next.starts_with('#')
                || next.starts_with("```")
                || next.starts_with('>')
                || is_hr(next)
                || list_marker(next).is_some()
                || (next.contains('|') && lines.get(i + 1).map(|n| is_table_separator(n)).unwrap_or(false))
            {
                break;
            }
            paragraph_lines.push(next.to_string());
            i += 1;
        }
        let paragraph_text = paragraph_lines.join("\n");
        tokens.push(BlockToken::Paragraph(tokenize_inline(&paragraph_text)));
    }

    tokens
}

fn is_hr(line: &str) -> bool {
    let trimmed = line.trim();
    trimmed.chars().all(|c| c == '-' || c == '*' || c == '_') && trimmed.chars().count() >= 3
}

fn is_table_separator(line: &str) -> bool {
    let trimmed = line.trim();
    trimmed.contains('|') && trimmed.chars().filter(|c| matches!(c, '-' | ':' | '|' | ' ')).count() == trimmed.chars().count()
}

fn split_table_row(line: &str) -> Vec<String> {
    line.split('|').filter(|cell| !cell.trim().is_empty() || line.starts_with('|')).map(|cell| cell.to_string()).collect()
}

/// Detect a list marker at line start. Returns (ordered, start_number, marker_char_length).
fn list_marker(line: &str) -> Option<(bool, f64, usize)> {
    let trimmed = line.trim_start();
    let indent = line.len() - trimmed.len();
    if indent > 3 {
        return None;
    }
    if trimmed
        .strip_prefix("- ")
        .or_else(|| trimmed.strip_prefix("* "))
        .or_else(|| trimmed.strip_prefix("+ "))
        .is_some()
    {
        return Some((false, 1.0, indent + 2));
    }
    // Ordered: digits followed by . or )
    let digit_count = trimmed.chars().take_while(|c| c.is_ascii_digit()).count();
    if digit_count > 0 && digit_count <= 9 {
        let after = &trimmed[digit_count..];
        if after
            .strip_prefix(". ")
            .or_else(|| after.strip_prefix(") "))
            .is_some()
        {
            let start: f64 = trimmed[..digit_count].parse().unwrap_or(1.0);
            return Some((true, start, indent + digit_count + 2));
        }
    }
    None
}

fn build_list_item(collected: &[(String, usize)], ordered: bool, indent: usize) -> ListItem {
    let raw = collected
        .iter()
        .map(|(content, _)| content.clone())
        .collect::<Vec<_>>()
        .join("\n");
    // Task list: [x] or [ ] at the start.
    let mut task = false;
    let mut checked = false;
    let mut content_lines = Vec::new();
    for (index, (content, line_indent)) in collected.iter().enumerate() {
        let mut line = content.clone();
        if index == 0 {
            if let Some(rest) = line.strip_prefix("[x] ") {
                task = true;
                checked = true;
                line = rest.to_string();
            } else if let Some(rest) = line.strip_prefix("[ ] ") {
                task = true;
                checked = false;
                line = rest.to_string();
            }
        }
        let _ = line_indent;
        content_lines.push(line);
    }
    let _ = indent;
    let _ = ordered;
    ListItem {
        raw,
        task,
        checked,
        tokens: tokenize_blocks(&content_lines.join("\n")),
    }
}

// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

pub struct MarkdownTheme {
    pub heading: Arc<dyn Fn(&str) -> String + Send + Sync>,
    pub bold: Arc<dyn Fn(&str) -> String + Send + Sync>,
    pub italic: Arc<dyn Fn(&str) -> String + Send + Sync>,
    pub underline: Arc<dyn Fn(&str) -> String + Send + Sync>,
    pub strikethrough: Arc<dyn Fn(&str) -> String + Send + Sync>,
    pub code: Arc<dyn Fn(&str) -> String + Send + Sync>,
    pub code_block: Arc<dyn Fn(&str) -> String + Send + Sync>,
    pub code_block_border: Arc<dyn Fn(&str) -> String + Send + Sync>,
    pub code_block_indent: String,
    pub quote: Arc<dyn Fn(&str) -> String + Send + Sync>,
    pub quote_border: Arc<dyn Fn(&str) -> String + Send + Sync>,
    pub link: Arc<dyn Fn(&str) -> String + Send + Sync>,
    pub link_url: Arc<dyn Fn(&str) -> String + Send + Sync>,
    pub list_bullet: Arc<dyn Fn(&str) -> String + Send + Sync>,
    pub hr: Arc<dyn Fn(&str) -> String + Send + Sync>,
    pub highlight_code: Option<Arc<dyn Fn(&str, &str) -> Vec<String> + Send + Sync>>,
}

pub struct DefaultTextStyle {
    pub color: Option<Arc<dyn Fn(&str) -> String + Send + Sync>>,
    pub bold: bool,
    pub italic: bool,
    pub strikethrough: bool,
    pub underline: bool,
    pub bg_color: Option<Arc<dyn Fn(&str) -> String + Send + Sync>>,
}

pub struct MarkdownOptions {
    pub render_latex: bool,
    pub preserve_backslash_escapes: bool,
    pub preserve_ordered_list_markers: bool,
    pub transform: Option<Arc<dyn Fn(&str, f64) -> String + Send + Sync>>,
}

impl Default for MarkdownOptions {
    fn default() -> Self {
        Self {
            render_latex: true,
            preserve_backslash_escapes: false,
            preserve_ordered_list_markers: false,
            transform: None,
        }
    }
}

pub struct Markdown {
    text: String,
    padding_x: usize,
    padding_y: usize,
    theme: MarkdownTheme,
    default_text_style: Option<DefaultTextStyle>,
    options: MarkdownOptions,
}

impl Markdown {
    pub fn new(
        text: &str,
        padding_x: usize,
        padding_y: usize,
        theme: MarkdownTheme,
        default_text_style: Option<DefaultTextStyle>,
        options: Option<MarkdownOptions>,
    ) -> Self {
        Self {
            text: text.to_string(),
            padding_x,
            padding_y,
            theme,
            default_text_style,
            options: options.unwrap_or_default(),
        }
    }

    pub fn set_text(&mut self, text: &str) {
        self.text = text.to_string();
    }

    fn apply_default_style(&self, text: &str) -> String {
        let Some(style) = &self.default_text_style else {
            return text.to_string();
        };
        let mut styled = text.to_string();
        if let Some(color) = &style.color {
            styled = color(&styled);
        }
        if style.bold {
            styled = (self.theme.bold)(&styled);
        }
        if style.italic {
            styled = (self.theme.italic)(&styled);
        }
        if style.strikethrough {
            styled = (self.theme.strikethrough)(&styled);
        }
        if style.underline {
            styled = (self.theme.underline)(&styled);
        }
        styled
    }

    fn style_prefix(&self, style_fn: &Arc<dyn Fn(&str) -> String + Send + Sync>) -> String {
        let styled = style_fn("\u{0}");
        match styled.find('\u{0}') {
            Some(index) => styled[..index].to_string(),
            None => String::new(),
        }
    }

    fn render_inline(&self, tokens: &[InlineToken], style_context: Option<&InlineContext>) -> String {
        let mut result = String::new();
        let apply_text: Arc<dyn Fn(&str) -> String + Send + Sync> = match style_context {
            Some(context) => context.apply_text.clone(),
            None => {
                let default = self.default_text_style.as_ref().map(|style| DefaultTextStyle {
                    color: style.color.clone(),
                    bold: style.bold,
                    italic: style.italic,
                    strikethrough: style.strikethrough,
                    underline: style.underline,
                    bg_color: None,
                });
                Arc::new(move |text| match &default {
                    Some(style) => {
                        let mut styled = text.to_string();
                        if let Some(color) = &style.color {
                            styled = color(&styled);
                        }
                        if style.bold {
                            styled = format!("\x1b[1m{styled}\x1b[22m");
                        }
                        if style.italic {
                            styled = format!("\x1b[3m{styled}\x1b[23m");
                        }
                        styled
                    }
                    None => text.to_string(),
                })
            }
        };
        let style_prefix = match style_context {
            Some(context) => context.style_prefix.clone(),
            None => String::new(),
        };

        let apply_with_newlines = |text: &str| -> String {
            text.split('\n')
                .map(|segment| apply_text(segment))
                .collect::<Vec<_>>()
                .join("\n")
        };

        for token in tokens {
            match token {
                InlineToken::Text(text) => result += &apply_with_newlines(text),
                InlineToken::Strong(content) => {
                    result += &(self.theme.bold)(&self.render_inline(&tokenize_inline(content), style_context));
                    result += &style_prefix;
                }
                InlineToken::Em(content) => {
                    result += &(self.theme.italic)(&self.render_inline(&tokenize_inline(content), style_context));
                    result += &style_prefix;
                }
                InlineToken::Codespan(text) => {
                    result += &(self.theme.code)(text);
                    result += &style_prefix;
                }
                InlineToken::Link { text, href } => {
                    let styled_link = (self.theme.link)(&(self.theme.underline)(text));
                    if get_capabilities().hyperlinks {
                        result += &format!("\x1b]8;;{href}\x07{styled_link}\x1b]8;;\x07");
                    } else {
                        let href_for_comparison = href.strip_prefix("mailto:").unwrap_or(href);
                        if text == href || text == href_for_comparison {
                            result += &styled_link;
                        } else {
                            result += &styled_link;
                            result += &(self.theme.link_url)(&format!(" ({href})"));
                        }
                    }
                    result += &style_prefix;
                }
                InlineToken::Del(content) => {
                    result += &(self.theme.strikethrough)(&self.render_inline(&tokenize_inline(content), style_context));
                    result += &style_prefix;
                }
                InlineToken::Br => result.push('\n'),
                InlineToken::Latex { text, raw } => {
                    let rendered = if self.options.render_latex {
                        render_latex(text, &crate::latex::RenderLatexOptions::default()).unwrap_or_else(|| raw.clone())
                    } else {
                        raw.clone()
                    };
                    result += &apply_with_newlines(&rendered);
                }
                InlineToken::Escape(text) => {
                    result += &apply_with_newlines(text);
                }
            }
        }
        while !style_prefix.is_empty() && result.ends_with(&style_prefix) {
            result.truncate(result.len() - style_prefix.len());
        }
        result
    }

    fn render_token(&self, token: &BlockToken, width: f64, next_type: Option<&str>) -> Vec<String> {
        let mut lines: Vec<String> = Vec::new();
        match token {
            BlockToken::Heading { depth, inline } => {
                let heading_prefix = format!("{} ", "#".repeat(*depth));
                let heading_style: Arc<dyn Fn(&str) -> String + Send + Sync> = if *depth == 1 {
                    let bold = self.theme.bold.clone();
                    let underline = self.theme.underline.clone();
                    let heading = self.theme.heading.clone();
                    Arc::new(move |text| heading(&bold(&underline(text))))
                } else {
                    let bold = self.theme.bold.clone();
                    let heading = self.theme.heading.clone();
                    Arc::new(move |text| heading(&bold(text)))
                };
                let prefix = self.style_prefix(&heading_style);
                let context = InlineContext {
                    apply_text: heading_style.clone(),
                    style_prefix: prefix,
                };
                let heading_text = self.render_inline(inline, Some(&context));
                let styled_heading = if *depth >= 3 {
                    heading_style(&heading_prefix) + &heading_text
                } else {
                    heading_text
                };
                lines.push(styled_heading);
                if next_type.is_some() && next_type != Some("space") {
                    lines.push(String::new());
                }
            }
            BlockToken::Paragraph(inline) => {
                lines.push(self.render_inline(inline, None));
                if next_type.is_some() && next_type != Some("list") && next_type != Some("space") {
                    lines.push(String::new());
                }
            }
            BlockToken::Text(inline) => {
                lines.push(self.render_inline(inline, None));
            }
            BlockToken::Code { lang, text } => {
                lines.push((self.theme.code_block_border)(&format!("```{lang}")));
                match &self.theme.highlight_code {
                    Some(highlight) => {
                        for line in highlight(text, lang) {
                            lines.push(format!("{}{line}", self.theme.code_block_indent));
                        }
                    }
                    None => {
                        for code_line in text.split('\n') {
                            lines.push(format!("{}{}", self.theme.code_block_indent, (self.theme.code_block)(code_line)));
                        }
                    }
                }
                lines.push((self.theme.code_block_border)("```"));
                if next_type.is_some() && next_type != Some("space") {
                    lines.push(String::new());
                }
            }
            BlockToken::List { ordered, start, loose, items } => {
                let mut index = 0.0;
                for (item_index, item) in items.iter().enumerate() {
                    let is_last = item_index == items.len() - 1;
                    let marker = if *ordered {
                        format!("{}. ", start + index)
                    } else {
                        "- ".to_string()
                    };
                    index += 1.0;
                    let task_marker = if item.task {
                        format!("[{}] ", if item.checked { "x" } else { " " })
                    } else {
                        String::new()
                    };
                    let marker_text = format!("{marker}{task_marker}");
                    let first_prefix = (self.theme.list_bullet)(&marker_text);
                    let continuation_prefix = " ".repeat(visible_width(&marker_text) as usize);
                    let item_width = (width - visible_width(&first_prefix)).max(1.0);
                    let mut rendered_any_line = false;
                    for item_token in &item.tokens {
                        if matches!(item_token, BlockToken::List { .. }) {
                            lines.extend(self.render_list(item_token, 1, width));
                            rendered_any_line = true;
                            continue;
                        }
                        for line in self.render_token(item_token, item_width, None) {
                            for wrapped in wrap_text_with_ansi(&line, item_width) {
                                let line_prefix = if rendered_any_line { &continuation_prefix } else { &first_prefix };
                                lines.push(format!("{line_prefix}{wrapped}"));
                                rendered_any_line = true;
                            }
                        }
                    }
                    if !rendered_any_line {
                        lines.push(first_prefix);
                    }
                    if *loose && !is_last {
                        lines.push(String::new());
                    }
                }
            }
            BlockToken::Table { header, rows, raw } => {
                lines.extend(self.render_table(header, rows, raw, width, next_type));
            }
            BlockToken::Blockquote(children) => {
                let quote_style: Arc<dyn Fn(&str) -> String + Send + Sync> = {
                    let theme = &self.theme;
                    let quote = theme.quote.clone();
                    let italic = theme.italic.clone();
                    Arc::new(move |text| quote(&italic(text)))
                };
                let quote_prefix = self.style_prefix(&quote_style);
                let quote_content_width = (width - 2.0).max(1.0);
                let context = InlineContext {
                    apply_text: Arc::new(|text| text.to_string()),
                    style_prefix: quote_prefix.clone(),
                };
                let mut rendered_quote_lines: Vec<String> = Vec::new();
                for (i, child) in children.iter().enumerate() {
                    let next = children.get(i + 1).map(|t| token_type_name(t));
                    rendered_quote_lines.extend(self.render_token(child, quote_content_width, next.as_deref()));
                }
                while rendered_quote_lines.last().map(|line| line.is_empty()).unwrap_or(false) {
                    rendered_quote_lines.pop();
                }
                let _ = context;
                for quote_line in rendered_quote_lines {
                    let styled = if quote_prefix.is_empty() {
                        quote_style(&quote_line)
                    } else {
                        quote_style(&quote_line.replace("\x1b[0m", &format!("\x1b[0m{quote_prefix}")))
                    };
                    for wrapped in wrap_text_with_ansi(&styled, quote_content_width) {
                        lines.push(format!("{}{wrapped}", (self.theme.quote_border)("│ ")));
                    }
                }
                if next_type.is_some() && next_type != Some("space") {
                    lines.push(String::new());
                }
            }
            BlockToken::Hr => {
                lines.push((self.theme.hr)(&"─".repeat(width.min(80.0) as usize)));
                if next_type.is_some() && next_type != Some("space") {
                    lines.push(String::new());
                }
            }
            BlockToken::Html(raw) => {
                lines.push(self.apply_default_style(raw.trim()));
            }
            BlockToken::Space => {
                lines.push(String::new());
            }
        }
        lines
    }

    fn render_list(&self, token: &BlockToken, depth: usize, width: f64) -> Vec<String> {
        match token {
            BlockToken::List { ordered, start, loose, items } => {
                let mut lines: Vec<String> = Vec::new();
                let indent = "    ".repeat(depth);
                let mut index = 0.0;
                for (item_index, item) in items.iter().enumerate() {
                    let is_last = item_index == items.len() - 1;
                    let marker = if *ordered {
                        format!("{}. ", start + index)
                    } else {
                        "- ".to_string()
                    };
                    index += 1.0;
                    let task_marker = if item.task {
                        format!("[{}] ", if item.checked { "x" } else { " " })
                    } else {
                        String::new()
                    };
                    let marker_text = format!("{marker}{task_marker}");
                    let first_prefix = format!("{indent}{}", (self.theme.list_bullet)(&marker_text));
                    let continuation_prefix = format!("{indent}{}", " ".repeat(visible_width(&marker_text) as usize));
                    let item_width = (width - visible_width(&first_prefix)).max(1.0);
                    let mut rendered_any_line = false;
                    for item_token in &item.tokens {
                        if matches!(item_token, BlockToken::List { .. }) {
                            lines.extend(self.render_list(item_token, depth + 1, width));
                            rendered_any_line = true;
                            continue;
                        }
                        for line in self.render_token(item_token, item_width, None) {
                            for wrapped in wrap_text_with_ansi(&line, item_width) {
                                let line_prefix = if rendered_any_line { &continuation_prefix } else { &first_prefix };
                                lines.push(format!("{line_prefix}{wrapped}"));
                                rendered_any_line = true;
                            }
                        }
                    }
                    if !rendered_any_line {
                        lines.push(first_prefix);
                    }
                    if *loose && !is_last {
                        lines.push(String::new());
                    }
                }
                lines
            }
            _ => Vec::new(),
        }
    }

    fn render_table(&self, header: &[Vec<InlineToken>], rows: &[Vec<Vec<InlineToken>>], raw: &str, width: f64, next_type: Option<&str>) -> Vec<String> {
        let mut lines: Vec<String> = Vec::new();
        let num_cols = header.len();
        if num_cols == 0 {
            return lines;
        }
        let border_overhead = 3.0 * num_cols as f64 + 1.0;
        let available_for_cells = width - border_overhead;
        if available_for_cells < num_cols as f64 {
            let fallback = wrap_text_with_ansi(raw, width);
            if next_type.is_some() && next_type != Some("space") {
                let mut result = fallback;
                result.push(String::new());
                return result;
            }
            return fallback;
        }
        let max_unbroken_word_width = 30.0;
        let mut natural_widths: Vec<f64> = Vec::new();
        let mut min_word_widths: Vec<f64> = Vec::new();
        for cell in header.iter() {
            let text = self.render_inline(cell, None);
            natural_widths.push(visible_width(&text));
            min_word_widths.push(get_longest_word_width(&text, max_unbroken_word_width));
        }
        for row in rows {
            for (i, cell) in row.iter().enumerate() {
                let text = self.render_inline(cell, None);
                if i < natural_widths.len() {
                    natural_widths[i] = natural_widths[i].max(visible_width(&text));
                    min_word_widths[i] = min_word_widths[i].max(get_longest_word_width(&text, max_unbroken_word_width));
                }
            }
        }

        let mut min_column_widths = min_word_widths.clone();
        let mut min_cells_width: f64 = min_column_widths.iter().sum();
        if min_cells_width > available_for_cells {
            min_column_widths = vec![1.0; num_cols];
            let remaining = available_for_cells - num_cols as f64;
            if remaining > 0.0 {
                let total_weight: f64 = min_word_widths.iter().map(|w| (w - 1.0).max(0.0)).sum();
                let growth: Vec<f64> = min_word_widths
                    .iter()
                    .map(|w| {
                        let weight = (w - 1.0).max(0.0);
                        if total_weight > 0.0 {
                            ((weight / total_weight) * remaining).floor()
                        } else {
                            0.0
                        }
                    })
                    .collect();
                for (i, growth_value) in growth.iter().enumerate() {
                    min_column_widths[i] += growth_value;
                }
                let allocated: f64 = growth.iter().sum();
                let mut leftover = remaining - allocated;
                for i in 0..num_cols {
                    if leftover <= 0.0 {
                        break;
                    }
                    min_column_widths[i] += 1.0;
                    leftover -= 1.0;
                }
            }
            min_cells_width = min_column_widths.iter().sum();
        }

        let total_natural_width: f64 = natural_widths.iter().sum::<f64>() + border_overhead;
        let column_widths: Vec<f64> = if total_natural_width <= width {
            natural_widths
                .iter()
                .enumerate()
                .map(|(index, natural)| natural.max(min_column_widths[index]))
                .collect()
        } else {
            let total_grow_potential: f64 = natural_widths
                .iter()
                .enumerate()
                .map(|(index, natural)| (natural - min_column_widths[index]).max(0.0))
                .sum();
            let extra_width = (available_for_cells - min_cells_width).max(0.0);
            let mut widths: Vec<f64> = min_column_widths
                .iter()
                .enumerate()
                .map(|(index, min_width)| {
                    let min_width_delta = (natural_widths[index] - min_width).max(0.0);
                    let grow = if total_grow_potential > 0.0 {
                        ((min_width_delta / total_grow_potential) * extra_width).floor()
                    } else {
                        0.0
                    };
                    min_width + grow
                })
                .collect();
            let allocated: f64 = widths.iter().sum();
            let mut remaining = available_for_cells - allocated;
            while remaining > 0.0 {
                let mut grew = false;
                for i in 0..num_cols {
                    if remaining <= 0.0 {
                        break;
                    }
                    if widths[i] < natural_widths[i] {
                        widths[i] += 1.0;
                        remaining -= 1.0;
                        grew = true;
                    }
                }
                if !grew {
                    break;
                }
            }
            widths
        };

        lines.push(format!(
            "┌─{}─┐",
            column_widths.iter().map(|w| "─".repeat(*w as usize)).collect::<Vec<_>>().join("─┬─")
        ));
        let header_cell_lines: Vec<Vec<String>> = header
            .iter()
            .enumerate()
            .map(|(i, cell)| {
                let text = self.render_inline(cell, None);
                wrap_text_with_ansi(&text, column_widths[i].max(1.0))
            })
            .collect();
        let header_line_count = header_cell_lines.iter().map(|lines| lines.len()).max().unwrap_or(0);
        for line_index in 0..header_line_count {
            let row_parts: Vec<String> = header_cell_lines
                .iter()
                .enumerate()
                .map(|(col, cell_lines)| {
                    let text = cell_lines.get(line_index).cloned().unwrap_or_default();
                    let padded = format!("{text}{}", " ".repeat(((column_widths[col] - visible_width(&text)).max(0.0)) as usize));
                    (self.theme.bold)(&padded)
                })
                .collect();
            lines.push(format!("│ {} │", row_parts.join(" │ ")));
        }
        let separator_line = format!(
            "├─{}─┤",
            column_widths.iter().map(|w| "─".repeat(*w as usize)).collect::<Vec<_>>().join("─┼─")
        );
        lines.push(separator_line.clone());
        for (row_index, row) in rows.iter().enumerate() {
            let row_cell_lines: Vec<Vec<String>> = row
                .iter()
                .enumerate()
                .map(|(i, cell)| {
                    let text = self.render_inline(cell, None);
                    wrap_text_with_ansi(&text, column_widths.get(i).copied().unwrap_or(1.0).max(1.0))
                })
                .collect();
            let row_line_count = row_cell_lines.iter().map(|lines| lines.len()).max().unwrap_or(0);
            for line_index in 0..row_line_count {
                let row_parts: Vec<String> = row_cell_lines
                    .iter()
                    .enumerate()
                    .map(|(col, cell_lines)| {
                        let text = cell_lines.get(line_index).cloned().unwrap_or_default();
                        format!("{text}{}", " ".repeat(((column_widths[col] - visible_width(&text)).max(0.0)) as usize))
                    })
                    .collect();
                lines.push(format!("│ {} │", row_parts.join(" │ ")));
            }
            if row_index < rows.len() - 1 {
                lines.push(separator_line.clone());
            }
        }
        lines.push(format!(
            "└─{}─┘",
            column_widths.iter().map(|w| "─".repeat(*w as usize)).collect::<Vec<_>>().join("─┴─")
        ));
        if next_type.is_some() && next_type != Some("space") {
            lines.push(String::new());
        }
        lines
    }
}

fn get_longest_word_width(text: &str, max_width: f64) -> f64 {
    let longest = text
        .split(|c: char| c.is_whitespace())
        .filter(|word| !word.is_empty())
        .map(visible_width)
        .fold(0.0, f64::max);
    longest.min(max_width).max(1.0)
}

fn token_type_name(token: &BlockToken) -> &'static str {
    match token {
        BlockToken::Heading { .. } => "heading",
        BlockToken::Paragraph(_) => "paragraph",
        BlockToken::Text(_) => "text",
        BlockToken::Code { .. } => "code",
        BlockToken::List { .. } => "list",
        BlockToken::Table { .. } => "table",
        BlockToken::Blockquote(_) => "blockquote",
        BlockToken::Hr => "hr",
        BlockToken::Html(_) => "html",
        BlockToken::Space => "space",
    }
}

struct InlineContext {
    apply_text: Arc<dyn Fn(&str) -> String + Send + Sync>,
    style_prefix: String,
}

impl Component for Markdown {
    fn render(&self, width: usize) -> Vec<String> {
        let content_width = (width as f64 - (self.padding_x as f64) * 2.0).max(1.0);
        let text = match &self.options.transform {
            Some(transform) => transform(&self.text, content_width),
            None => self.text.clone(),
        };
        if text.trim().is_empty() {
            return Vec::new();
        }
        let normalized = text.replace('\t', "   ");
        let tokens = tokenize_blocks(&normalized);
        let mut rendered_lines: Vec<String> = Vec::new();
        for (i, token) in tokens.iter().enumerate() {
            let next_type = tokens.get(i + 1).map(token_type_name);
            for line in self.render_token(token, content_width, next_type) {
                rendered_lines.push(line);
            }
        }
        let mut wrapped_lines: Vec<String> = Vec::new();
        for line in rendered_lines {
            if is_image_line(&line) {
                wrapped_lines.push(line);
            } else {
                for wrapped in wrap_text_with_ansi(&line, content_width) {
                    wrapped_lines.push(wrapped);
                }
            }
        }
        let left_margin = " ".repeat(self.padding_x);
        let right_margin = " ".repeat(self.padding_x);
        let bg_fn = self.default_text_style.as_ref().and_then(|style| style.bg_color.clone());
        let mut content_lines: Vec<String> = Vec::new();
        for line in wrapped_lines {
            if is_image_line(&line) {
                content_lines.push(line);
                continue;
            }
            let line_with_margins = format!("{left_margin}{line}{right_margin}");
            match &bg_fn {
                Some(bg) => content_lines.push(apply_background_to_line(&line_with_margins, width, bg.as_ref())),
                None => {
                    let visible_len = visible_width(&line_with_margins);
                    let padding_needed = ((width as f64) - visible_len).max(0.0) as usize;
                    content_lines.push(format!("{line_with_margins}{}", " ".repeat(padding_needed)));
                }
            }
        }
        let empty_line = " ".repeat(width);
        let mut result: Vec<String> = Vec::new();
        for _ in 0..self.padding_y {
            result.push(match &bg_fn {
                Some(bg) => apply_background_to_line(&empty_line, width, bg.as_ref()),
                None => empty_line.clone(),
            });
        }
        result.extend(content_lines);
        for _ in 0..self.padding_y {
            result.push(match &bg_fn {
                Some(bg) => apply_background_to_line(&empty_line, width, bg.as_ref()),
                None => empty_line.clone(),
            });
        }
        if result.is_empty() {
            vec![String::new()]
        } else {
            result
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity() -> Arc<dyn Fn(&str) -> String + Send + Sync> {
        Arc::new(|text| text.to_string())
    }

    fn theme() -> MarkdownTheme {
        MarkdownTheme {
            heading: identity(),
            bold: identity(),
            italic: identity(),
            underline: identity(),
            strikethrough: identity(),
            code: identity(),
            code_block: identity(),
            code_block_border: identity(),
            code_block_indent: "  ".to_string(),
            quote: identity(),
            quote_border: identity(),
            link: identity(),
            link_url: identity(),
            list_bullet: identity(),
            hr: identity(),
            highlight_code: None,
        }
    }

    fn render_markdown(text: &str) -> Vec<String> {
        let markdown = Markdown::new(text, 0, 0, theme(), None, None);
        markdown.render(60)
    }

    #[test]
    fn renders_headings() {
        let lines = render_markdown("# Title\n\nSome text");
        // h1/h2 render without the "# " prefix (JS: prefix only for h3+).
        assert!(lines.iter().any(|line| line.contains("Title")));
        assert!(lines.iter().any(|line| line.contains("Some text")));
        let lines = render_markdown("### H3");
        assert!(lines.iter().any(|line| line.contains("# ")));
    }

    #[test]
    fn renders_bold_italic_code() {
        let lines = render_markdown("**bold** and *italic* and `code`");
        assert!(lines[0].contains("bold"));
        assert!(lines[0].contains("italic"));
        assert!(lines[0].contains("code"));
    }

    #[test]
    fn renders_links() {
        let lines = render_markdown("[pi](https://pi.dev)");
        assert!(lines[0].contains("pi"));
    }

    #[test]
    fn renders_lists() {
        let lines = render_markdown("- one\n- two\n  - nested");
        assert!(lines.iter().any(|line| line.contains("one")));
        assert!(lines.iter().any(|line| line.contains("two")));
        assert!(lines.iter().any(|line| line.contains("nested")));
    }

    #[test]
    fn renders_ordered_lists() {
        let lines = render_markdown("1. first\n2. second");
        assert!(lines.iter().any(|line| line.contains("1.")));
        assert!(lines.iter().any(|line| line.contains("2.")));
    }

    #[test]
    fn renders_code_blocks() {
        let lines = render_markdown("```rust\nfn main() {}\n```");
        assert!(lines.iter().any(|line| line.contains("```rust")));
        assert!(lines.iter().any(|line| line.contains("fn main() {}")));
    }

    #[test]
    fn renders_blockquotes() {
        let lines = render_markdown("> quoted text");
        assert!(lines.iter().any(|line| line.contains("quoted text")));
    }

    #[test]
    fn renders_hr() {
        let lines = render_markdown("---");
        assert!(lines.iter().any(|line| line.contains('─')));
    }

    #[test]
    fn renders_tables() {
        let lines = render_markdown("| a | b |\n|---|---|\n| 1 | 2 |");
        assert!(lines.iter().any(|line| line.contains('a')));
        assert!(lines.iter().any(|line| line.contains('1')));
        assert!(lines.iter().any(|line| line.contains('│')));
    }

    #[test]
    fn renders_latex_inline() {
        let lines = render_markdown("$x^2$");
        assert!(lines.iter().any(|line| line.contains('x')));
    }

    #[test]
    fn empty_text_renders_nothing() {
        let markdown = Markdown::new("   ", 0, 0, theme(), None, None);
        assert!(markdown.render(60).is_empty());
    }
}
