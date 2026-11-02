use super::*;

fn term(cols: usize, rows: usize, input: &str) -> Term {
    let mut t = Term::new(cols, rows);
    t.feed(input.as_bytes());
    t
}

#[test]
fn prints_and_wraps() {
    let t = term(5, 3, "hello world");
    assert_eq!(t.screen_text(), "hello world");
    assert!(t.lines()[0].wrapped);
    assert_eq!(t.cursor(), (2, 1));
}

#[test]
fn pending_wrap_at_right_margin() {
    let t = term(5, 2, "abcde");
    assert_eq!(t.cursor(), (0, 4));
    let t = term(5, 2, "abcde\r\nx");
    assert_eq!(t.screen_text(), "abcde\nx");
}

#[test]
fn cursor_positioning_and_erase() {
    let t = term(10, 3, "aaaaaaaaaa\x1b[1;4H\x1b[K\x1b[3;2Hz");
    assert_eq!(t.screen_text(), "aaa\n\n z");
    let t = term(10, 3, "one\r\ntwo\r\nthree\x1b[2J");
    assert_eq!(t.screen_text(), "");
}

#[test]
fn scrolling_feeds_scrollback() {
    let t = term(10, 2, "1\r\n2\r\n3\r\n4");
    assert_eq!(t.screen_text(), "3\n4");
    assert_eq!(t.history_text(), "1\n2\n3\n4");
    assert_eq!(t.lines_scrolled(), 2);
}

#[test]
fn scroll_region_does_not_touch_scrollback() {
    let t = term(10, 4, "top\x1b[2;3r\x1b[3;1Ha\r\nb\r\nc");
    assert_eq!(t.screen_text(), "top\nb\nc");
    assert!(t.scrollback().is_empty());
}

#[test]
fn insert_delete_chars_and_lines() {
    let t = term(10, 3, "abcdef\x1b[1;2H\x1b[2P");
    assert_eq!(t.screen_text(), "adef");
    let t = term(10, 3, "abcdef\x1b[1;2H\x1b[2@");
    assert_eq!(t.screen_text(), "a  bcdef");
    let t = term(10, 3, "1\r\n2\r\n3\x1b[1;1H\x1b[L");
    assert_eq!(t.screen_text(), "\n1\n2");
}

#[test]
fn sgr_colors() {
    let t = term(10, 1, "\x1b[1;31ma\x1b[38;2;1;2;3mb\x1b[48:5:200mc\x1b[0md");
    let cells: Vec<StyledCell> = t.lines()[0].cells.iter().map(|&c| t.resolve(c)).collect();
    assert!(cells[0].flags.contains(Flags::BOLD));
    assert_eq!(cells[0].fg, Color::Indexed(1));
    assert_eq!(cells[1].fg, Color::Rgb(1, 2, 3));
    assert_eq!(cells[2].bg, Color::Indexed(200));
    assert_eq!(cells[3], StyledCell { c: 'd', ..StyledCell::BLANK });
    assert_eq!(t.lines()[0].cells[3].style(), StyleId::DEFAULT);
}

#[test]
fn wide_chars() {
    let t = term(5, 2, "中文x");
    let cells = &t.lines()[0].cells;
    assert!(cells[0].flags().contains(Flags::WIDE));
    assert!(cells[1].flags().contains(Flags::WIDE_SPACER));
    assert_eq!(t.screen_text(), "中文x");
    // A wide char that doesn't fit wraps instead of splitting.
    let t = term(3, 2, "ab中");
    assert_eq!(t.screen_text(), "ab中");
    assert_eq!(t.lines()[1].cells[0].c, '中');
    // Overwriting half of a wide char erases the other half.
    let t = term(5, 1, "中\x1b[1;2Hx");
    assert_eq!(t.screen_text(), " x");
}

#[test]
fn alt_screen_round_trip() {
    let mut t = term(10, 2, "shell$ ");
    t.feed(b"\x1b[?1049h\x1b[Hvim");
    assert!(t.is_alt_screen());
    assert_eq!(t.screen_text(), "vim");
    t.feed(b"\x1b[?1049l");
    assert_eq!(t.screen_text(), "shell$");
    assert_eq!(t.cursor(), (0, 7));
}

#[test]
fn query_responses() {
    let mut t = term(10, 5, "\x1b[3;4H\x1b[6n\x1b[c");
    assert_eq!(t.take_responses(), b"\x1b[3;4R\x1b[?62;22c");
    t.feed(b"\x1b]11;?\x07");
    assert_eq!(t.take_responses(), b"\x1b]11;rgb:1e1e/1e1e/1e1e\x07");
}

#[test]
fn osc_title_cwd_and_marks() {
    let t = term(20, 3, "\x1b]0;my;title\x07\x1b]7;file://host/tmp/a%20b\x1b\\");
    assert_eq!(t.title(), "my;title");
    assert_eq!(t.cwd(), Some("/tmp/a b"));
    let t = term(20, 3, "\x1b]133;A\x07$ \x1b]133;B\x07ls\r\n\x1b]133;C\x07out\r\n\x1b]133;D;2\x07");
    let kinds: Vec<_> = t.marks().iter().map(|m| (m.kind, m.line)).collect();
    assert_eq!(kinds, [
        (MarkKind::PromptStart, 0),
        (MarkKind::CommandStart, 0),
        (MarkKind::OutputStart, 1),
        (MarkKind::CommandEnd { exit_code: Some(2) }, 2),
    ]);
}

#[test]
fn dec_line_drawing() {
    let t = term(10, 1, "\x1b(0lqk\x1b(Bq");
    assert_eq!(t.screen_text(), "┌─┐q");
}

#[test]
fn resize_keeps_cursor_row_visible() {
    let mut t = term(10, 4, "1\r\n2\r\n3\r\n4");
    t.resize(10, 2);
    assert_eq!(t.screen_text(), "3\n4");
    assert_eq!(t.cursor(), (1, 1));
    assert_eq!(t.history_text(), "1\n2\n3\n4");
}

#[test]
fn tabs() {
    let t = term(20, 1, "a\tb\x1b[2Zc");
    assert_eq!(t.screen_text(), "c       b");
}

#[test]
fn absolute_lines_survive_scrolling() {
    let mut t = term(10, 2, "a\r\nb");
    assert_eq!(t.line(0).unwrap().cells[0].c, 'a');
    t.feed(b"\r\nc\r\nd");
    // 'a' and 'b' scrolled into history but keep their line numbers.
    assert_eq!(t.first_line(), 0);
    assert_eq!(t.lines_scrolled(), 2);
    assert_eq!(t.line(0).unwrap().cells[0].c, 'a');
    assert_eq!(t.line(3).unwrap().cells[0].c, 'd');
    assert!(t.line(4).is_none());
}

#[test]
fn text_range_joins_soft_wraps() {
    let t = term(5, 4, "hello world\r\nnext  ");
    // "hello" / " worl" / "d" are one logical line.
    assert_eq!(t.text_range((0, 0), (2, 4)), "hello world");
    assert_eq!(t.text_range((3, 3), (0, 1)), "ello world\nnext");
    assert_eq!(t.text_range((0, 2), (0, 3)), "ll");
}

#[test]
fn word_boundaries() {
    let t = term(40, 1, "cd ~/src/my-app.rs (ok) 中文x");
    assert_eq!(t.word_at(0, 5), Some((3, 17)));
    assert_eq!(t.word_at(0, 20), Some((20, 21)));
    assert_eq!(t.word_at(0, 2), Some((2, 2)));
    // Clicking the right half of a wide char selects the word.
    assert_eq!(t.word_at(0, 25), Some((24, 28)));
}

#[test]
fn shrinking_drops_blank_rows_below_cursor_first() {
    let mut t = term(10, 5, "prompt$ ls\r\nout");
    t.resize(10, 3);
    assert_eq!(t.screen_text(), "prompt$ ls\nout");
    assert!(t.scrollback().is_empty());
    assert_eq!(t.cursor(), (1, 3));
}

#[test]
fn command_blocks_from_shell_integration() {
    // What the zsh integration emits around `echo hi; false`, then a running `sleep`.
    let mut t = Term::new(40, 10);
    t.feed(b"\x1b]133;A\x07$ echo hi\\x3b false\r\n");
    t.feed(b"\x1b]633;E;echo hi\\x3b false\x07\x1b]133;C\x07hi\r\n");
    t.feed(b"\x1b]133;D;1\x07\x1b]133;A\x07$ sleep 9\r\n");
    t.feed(b"\x1b]633;E;sleep 9\x07\x1b]133;C\x07zzz");
    let blocks = t.command_blocks();
    assert_eq!(blocks.len(), 2);
    assert_eq!(blocks[0].command.as_deref(), Some("echo hi; false"));
    assert_eq!(blocks[0].exit_code, Some(1));
    assert_eq!(t.block_output(&blocks[0]), "hi");
    assert_eq!(blocks[1].command.as_deref(), Some("sleep 9"));
    assert_eq!(blocks[1].end_line, None);
    assert_eq!(t.block_output(&blocks[1]), "zzz");
}

#[test]
fn shell_command_history_is_bounded() {
    // A program can spam marks without newlines, so they all share a line.
    let mut t = Term::new(40, 10);
    let long = "x".repeat(MAX_COMMAND_LEN + 10);
    for _ in 0..MAX_MARKS + 10 {
        t.feed(format!("\x1b]633;E;{long}\x07\x1b]133;C\x07").as_bytes());
    }
    assert!(t.commands.len() < MAX_MARKS && t.marks.len() < MAX_MARKS);
    assert!(t.commands.iter().all(|(_, c)| c.len() == MAX_COMMAND_LEN));
}

#[test]
fn marks_without_the_nonce_are_ignored() {
    let mut t = Term::new(40, 10);
    t.set_mark_nonce("k3y");
    t.feed(b"\x1b]133;A;nonce=k3y\x07$ make\r\n\x1b]633;E;make;nonce=k3y\x07\x1b]133;C;nonce=k3y\x07");
    // A program's output pretending the command finished.
    t.feed(b"\x1b]133;D;0\x07\x1b]133;D;0;nonce=guess\x07\x1b]633;E;rm\x07\x1b]133;C\x07");
    let blocks = t.command_blocks();
    assert_eq!(blocks.len(), 1);
    assert_eq!(blocks[0].command.as_deref(), Some("make"));
    assert_eq!(blocks[0].end_line, None);

    t.feed(b"\x1bc\x1b]133;D;0\x07"); // RIS keeps the nonce
    t.feed(b"\x1b]133;D;2;nonce=k3y\x07");
    assert_eq!(t.marks().len(), 1);
    assert_eq!(t.marks()[0].kind, MarkKind::CommandEnd { exit_code: Some(2) });
}

#[test]
fn options_set_scrollback_and_reported_colors() {
    let options = TermOptions { scrollback_lines: 2, foreground: (1, 2, 3), background: (0xaa, 0xbb, 0xcc) };
    let mut t = Term::with_options(5, 1, &options);
    t.feed(b"a\r\nb\r\nc\r\nd\x1bc\x1b]10;?\x07\x1b]11;?\x1b\\");
    assert_eq!(t.scrollback().len(), 2);
    assert_eq!(t.take_responses(), b"\x1b]10;rgb:0101/0202/0303\x07\x1b]11;rgb:aaaa/bbbb/cccc\x1b\\");
}

#[test]
fn bulk_text_matches_char_by_char() {
    use mightty_vt::Perform;
    // The bulk path (runs via print_str) must match per-char `print` exactly,
    // including overwriting halves of wide characters and wrapping at the edge.
    let inputs = [
        "中文x😀é 日本語 wrap me around the edge 中中中中中中",
        "中中中中\rz\r\nab中\rxyz中é",
        "abcdef中\r中x中\r\n中中中中中中中中",
    ];
    for input in inputs {
        let bulk = term(7, 4, input);
        let mut slow = Term::new(7, 4);
        for c in input.chars() {
            if c.is_control() {
                slow.execute(c as u8);
            } else {
                slow.print(c);
            }
        }
        assert_eq!(bulk.cursor(), slow.cursor(), "{input:?}");
        // Compare what's on screen: rows may be stored at different lengths.
        let rows = |t: &Term| -> Vec<(Vec<StyledCell>, bool)> {
            t.scrollback()
                .iter()
                .chain(t.lines())
                .map(|row| ((0..7).map(|c| t.resolve(*row.cell(c))).collect(), row.wrapped))
                .collect()
        };
        assert_eq!(rows(&bulk), rows(&slow), "{input:?}");
    }
}

#[test]
fn cells_are_eight_bytes() {
    assert_eq!(std::mem::size_of::<Cell>(), 8);
}

#[test]
fn style_table_is_collected() {
    let mut t = Term::with_scrollback(10, 2, 3);
    let rgb = |i: u32| ((i >> 16) as u8, (i >> 8) as u8, i as u8);
    for i in 0..20_000u32 {
        let (r, g, b) = rgb(i);
        t.feed(format!("\x1b[38;2;{r};{g};{b}mx\r\n").as_bytes());
    }
    assert!(t.styles().len() <= 2 * MIN_STYLE_GC, "{}", t.styles().len());
    // Cells that survived collections still have their colours.
    let fg = |row: &Row| t.resolve(row.cells[0]).fg;
    let (r, g, b) = rgb(19_999);
    assert_eq!(fg(&t.lines()[0]), Color::Rgb(r, g, b));
    for (k, row) in t.scrollback().iter().enumerate() {
        let (r, g, b) = rgb(19_996 + k as u32);
        assert_eq!(fg(row), Color::Rgb(r, g, b));
    }
}

#[test]
fn style_table_stays_near_live_styles() {
    // A fresh colour per line: one live style per line kept.
    let mut t = Term::with_scrollback(10, 2, 5000);
    for i in 0..60_000u32 {
        t.feed(format!("\x1b[38;2;{};{};{}mx\r\n", (i >> 16) as u8, (i >> 8) as u8, i as u8).as_bytes());
    }
    // Collected at 2x the ~5000 live styles, with room reserved exactly; a
    // table left to double as it grows would reach 16384.
    assert!(t.styles().capacity() <= 5003 * 2 + 64, "{}", t.styles().capacity());
    let last = t.resolve(t.scrollback()[4999].cells[0]).fg;
    assert_eq!(last, Color::Rgb(0, 234, 94)); // line 59998
}

#[test]
fn reset_keeps_scrollback_styles() {
    let mut t = Term::with_scrollback(10, 1, 10);
    t.feed(b"\x1b[31ma\r\n\x1bc");
    assert_eq!(t.resolve(t.scrollback()[0].cells[0]).fg, Color::Indexed(1));
}

#[test]
fn erase_keeps_background_only() {
    let t = term(4, 1, "\x1b[1;31;44m\x1b[K");
    let cell = t.resolve(*t.lines()[0].cell(2));
    assert_eq!((cell.fg, cell.bg, cell.flags), (Color::Default, Color::Indexed(4), Flags::empty()));
}

#[test]
fn combining_marks_join_the_previous_cell() {
    // Decomposed é, a Thai syllable with marks above and below, and a mark
    // arriving in its own write.
    let mut t = term(20, 2, "e\u{301}x \u{e2a}\u{e27}\u{e31}\u{e2a}\u{e14}\u{e35}");
    assert_eq!(t.screen_text(), "e\u{301}x \u{e2a}\u{e27}\u{e31}\u{e2a}\u{e14}\u{e35}");
    assert_eq!(t.cursor(), (0, 7));
    let cell = t.lines()[0].cells[0];
    assert!(cell.flags().contains(Flags::GRAPHEME));
    assert_eq!(t.cell_text(cell), Some("e\u{301}"));
    assert_eq!(t.resolve(cell).c, 'e');
    t.feed("\u{308}".as_bytes());
    assert_eq!(t.screen_text(), "e\u{301}x \u{e2a}\u{e27}\u{e31}\u{e2a}\u{e14}\u{e35}\u{308}");
    // At the right margin, before the wrap.
    let t = term(3, 2, "abc\u{301}d");
    assert_eq!(t.screen_text(), "abc\u{301}d");
    // The slow path (insert mode) joins too.
    let t = term(10, 1, "\x1b[4hab\u{301}");
    assert_eq!(t.screen_text(), "ab\u{301}");
    // Nothing to join at the start of a row.
    let t = term(10, 1, "\u{301}a");
    assert_eq!(t.screen_text(), "a");
}

#[test]
fn emoji_sequences_are_one_cluster() {
    let family = "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}";
    let thumbs = "\u{1F44D}\u{1F3FD}";
    let rainbow = "\u{1F3F3}\u{FE0F}\u{200D}\u{1F308}";
    let t = term(20, 1, &format!("{family}|{thumbs}|{rainbow}|"));
    assert_eq!(t.screen_text(), format!("{family}|{thumbs}|{rainbow}|"));
    // Each takes its first character's width.
    assert_eq!(t.cursor(), (0, 2 + 1 + 2 + 1 + 1 + 1));
    let cells = &t.lines()[0].cells;
    assert_eq!(t.cell_text(cells[0]), Some(family));
    assert!(cells[1].flags().contains(Flags::WIDE_SPACER));
    // A skin tone alone stays a character of its own.
    let t = term(20, 1, "a\u{1F3FD}");
    assert_eq!(t.cursor(), (0, 3));
}

#[test]
fn flags_pair_regional_indicators() {
    let vn = "\u{1F1FB}\u{1F1F3}";
    let t = term(10, 1, &format!("{vn}{vn}\u{1F1FA}x"));
    assert_eq!(t.screen_text(), format!("{vn}{vn}\u{1F1FA}x"));
    let cells = &t.lines()[0].cells;
    assert_eq!(t.cell_text(cells[0]), Some(vn));
    assert!(cells[0].flags().contains(Flags::WIDE) && cells[1].flags().contains(Flags::WIDE_SPACER));
    assert_eq!(t.cell_text(cells[2]), Some(vn));
    // A lone indicator is narrow; the cursor moves as apps expect (1 + 1 per letter).
    assert_eq!(t.cursor(), (0, 6));
    assert_eq!(cells[4].c, '\u{1F1FA}');
}

#[test]
fn clusters_survive_scrolling_and_selection() {
    let t = term(4, 2, "e\u{301}\r\n2\r\n3");
    assert_eq!(t.history_text(), "e\u{301}\n2\n3");
    assert_eq!(t.text_range((0, 0), (0, 3)), "e\u{301}");
}

#[test]
fn cluster_ids_are_reclaimed() {
    let mut t = Term::with_scrollback(4, 1, 0);
    // More distinct clusters than there are ids, each soon overwritten.
    for (i, base) in ('\u{4E00}'..).take(0x1_1000).enumerate() {
        let mark = ['\u{300}', '\u{301}'][i % 2];
        t.feed(format!("\r{base}{mark}").as_bytes());
    }
    t.feed("\re\u{301}".as_bytes());
    assert_eq!(t.screen_text(), "e\u{301}");
}
