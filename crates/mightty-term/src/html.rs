//! Selections as HTML, with their colours and styles, for the clipboard:
//! apps that paste rich text (Notes, Mail, chat) keep how it looked.

use std::fmt::Write;

use crate::cell::{Color, Flags, Underline};
use crate::colors::{Colors, Rgb};
use crate::{Piece, Term};

/// Link schemes kept as links. Others (`javascript:`, `file:`) are left as
/// plain text: what a program printed mustn't run where it's pasted.
const LINK_SCHEMES: &[&str] = &["http://", "https://", "mailto:"];

impl Term {
    /// [`Term::text_range`] as HTML: the same text, coloured as drawn.
    pub fn html_range(&self, a: (u64, usize), b: (u64, usize)) -> String {
        self.html(&self.range_pieces(a, b))
    }

    /// [`Term::text_block`] as HTML.
    pub fn html_block(&self, a: (u64, usize), b: (u64, usize)) -> String {
        self.html(&self.block_pieces(a, b))
    }

    fn html(&self, pieces: &[Piece]) -> String {
        let colors = self.colors();
        let mut out = format!(
            "<meta charset=\"utf-8\"><pre style=\"font-family:ui-monospace,Menlo,monospace;\
             color:{};background-color:{}\">",
            hex(colors.foreground),
            hex(colors.background)
        );
        for piece in pieces {
            let mut cells = piece.cells.as_slice();
            if piece.trim {
                while let [rest @ .., last] = cells
                    && self.cell_blank(*last)
                {
                    cells = rest;
                }
            }
            // A span per run of cells in one style.
            for run in cells.chunk_by(|a, b| a.style() == b.style()) {
                let style = self.style(run[0]);
                let css = css(style, &colors);
                let link = Some(style.link)
                    .filter(|&id| id != 0)
                    .and_then(|id| self.link(id))
                    .filter(|url| LINK_SCHEMES.iter().any(|s| url.starts_with(s)));
                if let Some(url) = link {
                    let _ = write!(out, "<a href=\"{}\">", escape(url));
                }
                if !css.is_empty() {
                    let _ = write!(out, "<span style=\"{css}\">");
                }
                let mut text = String::new();
                for &cell in run {
                    self.push_cell(&mut text, cell);
                }
                out += &escape(&text);
                if !css.is_empty() {
                    out += "</span>";
                }
                if link.is_some() {
                    out += "</a>";
                }
            }
            if piece.newline {
                out.push('\n');
            }
        }
        out += "</pre>";
        out
    }

    /// Whether `cell` copies as a space, which line ends drop.
    fn cell_blank(&self, cell: crate::Cell) -> bool {
        !cell.flags().contains(Flags::GRAPHEME)
            && (cell.c == ' ' || self.style(cell).flags.contains(Flags::HIDDEN))
    }
}

/// The CSS for text in `style`, beyond what the `<pre>` around it sets.
fn css(style: &crate::Style, colors: &Colors) -> String {
    let resolve = |c: Color, default: Rgb| match c {
        Color::Default => default,
        Color::Indexed(i) => colors.palette[usize::from(i)],
        Color::Rgb(r, g, b) => (r, g, b),
    };
    let flags = style.flags;
    let mut fg = resolve(style.fg, colors.foreground);
    let mut bg = resolve(style.bg, colors.background);
    if flags.contains(Flags::INVERSE) {
        (fg, bg) = (bg, fg);
    }
    if flags.contains(Flags::DIM) {
        // Drawn at half opacity over its background.
        let mix = |f: u8, b: u8| ((u16::from(f) + u16::from(b)) / 2) as u8;
        fg = (mix(fg.0, bg.0), mix(fg.1, bg.1), mix(fg.2, bg.2));
    }
    let mut css = Vec::new();
    if fg != colors.foreground {
        css.push(format!("color:{}", hex(fg)));
    }
    if bg != colors.background {
        css.push(format!("background-color:{}", hex(bg)));
    }
    if flags.contains(Flags::BOLD) {
        css.push("font-weight:bold".into());
    }
    if flags.contains(Flags::ITALIC) {
        css.push("font-style:italic".into());
    }
    let lines: Vec<_> = [
        (Flags::UNDERLINE, "underline"),
        (Flags::STRIKE, "line-through"),
    ]
    .into_iter()
    .filter(|&(flag, _)| flags.contains(flag))
    .map(|(_, line)| line)
    .collect();
    if !lines.is_empty() {
        css.push(format!("text-decoration-line:{}", lines.join(" ")));
    }
    if flags.contains(Flags::UNDERLINE) {
        match style.underline() {
            Underline::Single => {}
            Underline::Double => css.push("text-decoration-style:double".into()),
            Underline::Curly => css.push("text-decoration-style:wavy".into()),
            Underline::Dotted => css.push("text-decoration-style:dotted".into()),
            Underline::Dashed => css.push("text-decoration-style:dashed".into()),
        }
        if style.underline_color != Color::Default {
            let color = resolve(style.underline_color, colors.foreground);
            css.push(format!("text-decoration-color:{}", hex(color)));
        }
    }
    css.join(";")
}

fn hex((r, g, b): Rgb) -> String {
    format!("#{r:02x}{g:02x}{b:02x}")
}

/// `text` safe in HTML text and in a quoted attribute.
fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out += "&amp;",
            '<' => out += "&lt;",
            '>' => out += "&gt;",
            '"' => out += "&quot;",
            c => out.push(c),
        }
    }
    out
}
