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
    assert_eq!(
        cells[3],
        StyledCell {
            c: 'd',
            ..StyledCell::BLANK
        }
    );
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
    let t = term(
        20,
        3,
        "\x1b]0;my;title\x07\x1b]7;file://host/tmp/a%20b\x1b\\",
    );
    assert_eq!(t.title(), "my;title");
    assert_eq!(t.cwd(), Some("/tmp/a b"));
    assert_eq!(t.cwd_host(), "host");
    let t = term(
        20,
        3,
        "\x1b]133;A\x07$ \x1b]133;B\x07ls\r\n\x1b]133;C\x07out\r\n\x1b]133;D;2\x07",
    );
    let kinds: Vec<_> = t.marks().iter().map(|m| (m.kind, m.line)).collect();
    assert_eq!(
        kinds,
        [
            (MarkKind::PromptStart, 0),
            (MarkKind::CommandStart, 0),
            (MarkKind::OutputStart, 1),
            (MarkKind::CommandEnd { exit_code: Some(2) }, 2),
        ]
    );
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
fn text_blocks_are_columns() {
    let mut t = Term::new(12, 4);
    t.feed("name  size\r\nfoo   12\r\nbar   3456\r\n中文x".as_bytes());
    // Columns 6..=9, either corner first; lines keep their place even when blank.
    assert_eq!(t.text_block((0, 6), (2, 9)), "size\n12\n3456");
    assert_eq!(t.text_block((2, 9), (0, 6)), "size\n12\n3456");
    assert_eq!(t.text_block((0, 0), (1, 2)), "nam\nfoo");
    // Wide characters starting inside count; trailing blanks trimmed.
    assert_eq!(t.text_block((3, 0), (3, 4)), "中文x");
    assert_eq!(t.text_block((3, 1), (3, 3)), "文");
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

    let started = |c: &str| CommandEvent::Started {
        command: Some(c.into()),
    };
    let finished = CommandEvent::Finished {
        command: Some("echo hi; false".into()),
        exit_code: Some(1),
    };
    assert_eq!(
        t.take_command_events(),
        [started("echo hi; false"), finished, started("sleep 9")]
    );
    // An empty command line (prompt, then the next prompt) is no command.
    t.feed(b"\x1b]133;D;0\x07\x1b]133;A\x07$ \x1b]133;B\x07\x1b]133;D\x07\x1b]133;A\x07$ ");
    let finished = CommandEvent::Finished {
        command: Some("sleep 9".into()),
        exit_code: Some(0),
    };
    assert_eq!(t.take_command_events(), [finished]);
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
    t.feed(
        b"\x1b]133;A;nonce=k3y\x07$ make\r\n\x1b]633;E;make;nonce=k3y\x07\x1b]133;C;nonce=k3y\x07",
    );
    // A program's output pretending the command finished.
    t.feed(b"\x1b]133;D;0\x07\x1b]133;D;0;nonce=guess\x07\x1b]633;E;rm\x07\x1b]133;C\x07");
    let blocks = t.command_blocks();
    assert_eq!(blocks.len(), 1);
    assert_eq!(blocks[0].command.as_deref(), Some("make"));
    assert_eq!(blocks[0].end_line, None);

    t.feed(b"\x1bc\x1b]133;D;0\x07"); // RIS keeps the nonce
    t.feed(b"\x1b]133;D;2;nonce=k3y\x07");
    assert_eq!(t.marks().len(), 1);
    assert_eq!(
        t.marks()[0].kind,
        MarkKind::CommandEnd { exit_code: Some(2) }
    );
}

#[test]
fn output_cant_choose_the_directory_or_inject_escapes() {
    let mut t = Term::new(40, 10);
    t.set_mark_nonce("k3y");
    t.feed(b"\x1b]7;file://host/home/me;nonce=k3y\x07");
    // Without the nonce, a directory from output is ignored.
    t.feed(b"\x1b]7;file://host/tmp/evil\x07");
    assert_eq!(t.cwd(), Some("/home/me"));
    // Control characters in a path are refused even with it.
    t.feed(b"\x1b]7;file://host/tmp/%1b]52%3bc%3bZXZpbA%07;nonce=k3y\x07");
    assert_eq!(t.cwd(), None);
    // Titles and command lines lose control and bidi characters.
    t.feed("\x1b]2;a\u{9b}31mb\u{202e}c\x07".as_bytes());
    assert_eq!(t.title(), "a\u{FFFD}31mb\u{FFFD}c");
    t.feed(b"\x1b]633;E;rm\\x1b]0\\x3bpwn\\x07;nonce=k3y\x07\x1b]133;C;nonce=k3y\x07");
    assert_eq!(
        t.command_blocks().last().unwrap().command.as_deref(),
        Some("rm\u{FFFD}]0;pwn\u{FFFD}")
    );
    t.feed(format!("\x1b]2;{}\x07", "x".repeat(5000)).as_bytes());
    assert_eq!(t.title().chars().count(), 1024);
}

#[test]
fn hidden_text_isnt_copied() {
    let t = term(40, 2, "pip install foo\x1b[8m;curl x|sh\x1b[28m!");
    assert_eq!(
        t.text_range((0, 0), (0, 39)).trim_end(),
        format!("pip install foo{}!", " ".repeat(";curl x|sh".len()))
    );
}

#[test]
fn options_set_scrollback_and_reported_colors() {
    let colors = Colors {
        foreground: (1, 2, 3),
        background: (0xaa, 0xbb, 0xcc),
        ..Colors::default()
    };
    let options = TermOptions {
        scrollback_lines: 2,
        colors,
        ..TermOptions::default()
    };
    let mut t = Term::with_options(5, 1, &options);
    t.feed(b"a\r\nb\r\nc\r\nd\x1bc\x1b]10;?\x07\x1b]11;?\x1b\\");
    assert_eq!(t.scrollback().len(), 2);
    assert_eq!(
        t.take_responses(),
        b"\x1b]10;rgb:0101/0202/0303\x07\x1b]11;rgb:aaaa/bbbb/cccc\x1b\\"
    );
}

#[test]
fn programs_set_and_reset_colors() {
    let colors = Colors {
        foreground: (1, 2, 3),
        background: (4, 5, 6),
        ..Colors::default()
    };
    let mut t = Term::with_options(
        5,
        1,
        &TermOptions {
            colors,
            ..TermOptions::default()
        },
    );
    // The palette: several entries at once, asked about in between.
    t.feed(b"\x1b]4;1;rgb:ff/00/00;2;?;300;#fff\x1b\\");
    assert_eq!(t.take_responses(), b"\x1b]4;2;rgb:9898/c3c3/7979\x1b\\");
    assert_eq!(t.colors().palette[1], (255, 0, 0));
    // Foreground, then the next ones: background and cursor.
    t.feed(b"\x1b]10;#102030;#405060;rgb:7/8/9\x07\x1b]12;?\x07");
    let c = t.colors();
    assert_eq!(
        (c.foreground, c.background, c.cursor),
        (
            (0x10, 0x20, 0x30),
            (0x40, 0x50, 0x60),
            Some((0x77, 0x88, 0x99))
        )
    );
    assert_eq!(t.take_responses(), b"\x1b]12;rgb:7777/8888/9999\x07");
    // A reload keeps what programs set.
    t.set_options(&TermOptions::default());
    assert_eq!(t.colors().foreground, (0x10, 0x20, 0x30));
    // Resets: one, then all.
    t.feed(b"\x1b]4;3;#010101\x07\x1b]104;1\x07\x1b]110\x07\x1b]112\x07");
    let c = t.colors();
    assert_eq!(
        (c.palette[1], c.palette[3], c.foreground, c.cursor),
        (default_palette(1), (1, 1, 1), DEFAULT_FG, None)
    );
    // The cursor following the text reports the foreground.
    t.feed(b"\x1b]12;?\x07");
    assert_eq!(t.take_responses(), b"\x1b]12;rgb:d8d8/d8d8/d8d8\x07");
    t.feed(b"\x1b]104\x07");
    assert_eq!(t.colors().palette[3], default_palette(3));
    // RIS forgets them, and keeps the theme's.
    t.set_options(&TermOptions {
        colors: Colors {
            background: (9, 9, 9),
            ..Colors::default()
        },
        ..TermOptions::default()
    });
    t.feed(b"\x1b]11;#000000\x07\x1bc");
    assert_eq!(t.colors().background, (9, 9, 9));
}

#[test]
fn xtversion() {
    let mut t = Term::new(5, 1);
    t.feed(b"\x1b[>q");
    assert_eq!(
        t.take_responses(),
        format!("\x1bP>|mightty {}\x1b\\", env!("CARGO_PKG_VERSION")).as_bytes()
    );
}

#[test]
fn osc52_clipboard_requests() {
    let mut t = Term::new(10, 2);
    t.feed(b"\x1b]52;c;aMOpbGxvIOKckw==\x07"); // "héllo ✓"
    t.feed(b"\x1b]52;;?\x1b\\");
    t.feed(b"\x1b]52;c;not base64!\x07"); // ignored
    assert_eq!(
        t.take_clipboard_requests(),
        [
            ClipboardRequest::Write("héllo ✓".into()),
            ClipboardRequest::Read {
                bell_terminated: false
            }
        ]
    );
    assert!(t.take_clipboard_requests().is_empty());
    assert_eq!(clipboard_reply("hi", true), b"\x1b]52;c;aGk=\x07");
    for _ in 0..20 {
        t.feed(b"\x1b]52;c;?\x07");
    }
    assert_eq!(t.take_clipboard_requests().len(), 8);
}

#[test]
fn clear_above_cursor_keeps_the_prompt_line() {
    let mut t = Term::new(10, 3);
    t.feed(b"one\r\ntwo\r\nthree\r\nfour\r\n$ ls");
    assert_eq!(t.scrollback().len(), 2);
    let scrolled = t.lines_scrolled();
    t.clear_above_cursor();
    assert!(t.scrollback().is_empty());
    assert_eq!(t.cursor(), (0, 4));
    assert_eq!(t.lines_scrolled(), scrolled + 2);
    t.feed(b"\x1b[?1049h");
    t.feed(b"\r\nx");
    t.clear_above_cursor(); // the alternate screen is left alone
    assert_eq!(t.cursor().0, 1);
}

#[test]
fn set_options_changes_a_running_terminal() {
    let mut t = Term::with_options(
        5,
        1,
        &TermOptions {
            scrollback_lines: 10,
            ..TermOptions::default()
        },
    );
    t.feed(b"a\r\nb\r\nc\r\nd");
    assert_eq!(t.scrollback().len(), 3);
    let colors = Colors {
        foreground: (1, 2, 3),
        background: (4, 5, 6),
        ..Colors::default()
    };
    t.set_options(&TermOptions {
        scrollback_lines: 1,
        colors,
        ..TermOptions::default()
    });
    assert_eq!(t.scrollback().len(), 1);
    t.feed(b"\x1b]10;?\x07\x1b]11;?\x07");
    assert_eq!(
        t.take_responses(),
        b"\x1b]10;rgb:0101/0202/0303\x07\x1b]11;rgb:0404/0505/0606\x07"
    );
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
                .map(|row| {
                    (
                        (0..7).map(|c| t.resolve(*row.cell(c))).collect(),
                        row.wrapped,
                    )
                })
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
        t.feed(
            format!(
                "\x1b[38;2;{};{};{}mx\r\n",
                (i >> 16) as u8,
                (i >> 8) as u8,
                i as u8
            )
            .as_bytes(),
        );
    }
    // Collected at 2x the ~5000 live styles, with room reserved exactly; a
    // table left to double as it grows would reach 16384.
    assert!(
        t.styles().capacity() <= 5003 * 2 + 64,
        "{}",
        t.styles().capacity()
    );
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
    assert_eq!(
        (cell.fg, cell.bg, cell.flags),
        (Color::Default, Color::Indexed(4), Flags::empty())
    );
}

#[test]
fn combining_marks_join_the_previous_cell() {
    // Decomposed é, a Thai syllable with marks above and below, and a mark
    // arriving in its own write.
    let mut t = term(
        20,
        2,
        "e\u{301}x \u{e2a}\u{e27}\u{e31}\u{e2a}\u{e14}\u{e35}",
    );
    assert_eq!(
        t.screen_text(),
        "e\u{301}x \u{e2a}\u{e27}\u{e31}\u{e2a}\u{e14}\u{e35}"
    );
    assert_eq!(t.cursor(), (0, 7));
    let cell = t.lines()[0].cells[0];
    assert!(cell.flags().contains(Flags::GRAPHEME));
    assert_eq!(t.cell_text(cell), Some("e\u{301}"));
    assert_eq!(t.resolve(cell).c, 'e');
    t.feed("\u{308}".as_bytes());
    assert_eq!(
        t.screen_text(),
        "e\u{301}x \u{e2a}\u{e27}\u{e31}\u{e2a}\u{e14}\u{e35}\u{308}"
    );
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
    // Each takes its first character's width, the white flag widened by VS16.
    assert_eq!(t.cursor(), (0, 2 + 1 + 2 + 1 + 2 + 1));
    let cells = &t.lines()[0].cells;
    assert_eq!(t.cell_text(cells[0]), Some(family));
    assert!(cells[1].flags().contains(Flags::WIDE_SPACER));
    // A skin tone alone stays a character of its own.
    let t = term(20, 1, "a\u{1F3FD}");
    assert_eq!(t.cursor(), (0, 3));
}

#[test]
fn vs16_makes_emoji_wide() {
    let heart = "\u{2764}\u{FE0F}";
    let keycap = "1\u{FE0F}\u{20E3}";
    let t = term(10, 1, &format!("{heart}|{keycap}|\u{2764}|"));
    assert_eq!(t.screen_text(), format!("{heart}|{keycap}|\u{2764}|"));
    let cells = &t.lines()[0].cells;
    assert_eq!(t.cell_text(cells[0]), Some(heart));
    assert!(
        cells[0].flags().contains(Flags::WIDE) && cells[1].flags().contains(Flags::WIDE_SPACER)
    );
    assert_eq!(t.cell_text(cells[3]), Some(keycap));
    assert!(cells[3].flags().contains(Flags::WIDE));
    // Without VS16 the heart stays narrow.
    assert_eq!(t.cursor(), (0, 2 + 1 + 2 + 1 + 1 + 1));
    // Not for characters without an emoji style, nor emoji already wide.
    let t = term(10, 1, "a\u{FE0F}\u{1F600}\u{FE0F}|");
    assert_eq!(t.cursor(), (0, 1 + 2 + 1));
    // At the right margin it can't grow.
    let t = term(3, 2, &format!("ab{heart}"));
    assert_eq!(t.cursor(), (0, 2));
    assert_eq!(t.cell_text(t.lines()[0].cells[2]), Some(heart));
    // Mode 2027 reset: the first character's width, as wcwidth counts.
    let t = term(10, 1, &format!("\x1b[?2027l{heart}|"));
    assert_eq!(t.cursor(), (0, 2));
    // grapheme-width = legacy: reset from the start, after RIS too, until set.
    let legacy = TermOptions {
        wide_emoji: false,
        ..TermOptions::default()
    };
    let mut t = Term::with_options(10, 1, &legacy);
    t.feed(format!("{heart}|\x1bc{heart}|").as_bytes());
    assert_eq!(t.cursor(), (0, 2));
    t.feed(format!("\x1b[?2027h\r{heart}|").as_bytes());
    assert_eq!(t.cursor(), (0, 3));
    // Changed on a running terminal, it applies from then on.
    t.set_options(&TermOptions::default());
    t.feed(b"\x1bc\x1b[?2027$p");
    assert_eq!(t.take_responses(), b"\x1b[?2027;1$y");
}

#[test]
fn mode_reports() {
    let mut t = term(
        10,
        2,
        "\x1b[?2027$p\x1b[?2004h\x1b[?2004$p\x1b[?25l\x1b[?25$p\x1b[4$p\x1b[?9999$p\x1b[?1049h\x1b[?1049$p",
    );
    assert_eq!(
        t.take_responses(),
        b"\x1b[?2027;1$y\x1b[?2004;1$y\x1b[?25;2$y\x1b[4;2$y\x1b[?9999;0$y\x1b[?1049;1$y"
    );
    t.feed(b"\x1b[?2027l\x1b[?2027$p\x1b[?1002h\x1b[?1000$p\x1b[?1002$p");
    assert_eq!(
        t.take_responses(),
        b"\x1b[?2027;2$y\x1b[?1000;2$y\x1b[?1002;1$y"
    );
}

#[test]
fn flags_pair_regional_indicators() {
    let vn = "\u{1F1FB}\u{1F1F3}";
    let t = term(10, 1, &format!("{vn}{vn}\u{1F1FA}x"));
    assert_eq!(t.screen_text(), format!("{vn}{vn}\u{1F1FA}x"));
    let cells = &t.lines()[0].cells;
    assert_eq!(t.cell_text(cells[0]), Some(vn));
    assert!(
        cells[0].flags().contains(Flags::WIDE) && cells[1].flags().contains(Flags::WIDE_SPACER)
    );
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

#[test]
fn clusters_are_capped() {
    let family = "\u{1F468}\u{1F3FD}\u{200D}\u{1F469}\u{1F3FD}\u{200D}\u{1F467}\u{1F3FD}\u{200D}\u{1F466}\u{1F3FD}";
    let t = term(10, 1, family);
    assert_eq!(t.screen_text(), family);
    // A flood of marks stops growing the cell at the cap.
    let t = term(10, 1, &format!("e{}x", "\u{301}".repeat(100_000)));
    let text = t.screen_text();
    assert!(
        text.len() <= MAX_CLUSTER_BYTES + 1 && text.starts_with("e\u{301}") && text.ends_with('x'),
        "{}",
        text.len()
    );
    assert_eq!(t.cursor(), (0, 2));
}

#[test]
fn search_finds_matches_across_scrollback_and_wraps() {
    // Lines 0-1 end up in scrollback; "needle" on line 2 soft-wraps onto line 3.
    let t = term(
        10,
        3,
        "a needle\r\nold\r\nxxxxxxneedle Needle\r\n\u{6f22}needle",
    );
    let found = t.search("needle");
    assert_eq!(
        found,
        vec![
            ((0, 2), (0, 7)),
            ((2, 6), (3, 1)),
            ((3, 3), (3, 8)),
            ((4, 2), (4, 7))
        ]
    );
    // A capital makes the search case-sensitive.
    assert_eq!(t.search("Needle"), vec![((3, 3), (3, 8))]);
    assert!(t.search("").is_empty() && t.search("absent").is_empty());
    // A wide character's match covers its second cell.
    assert_eq!(t.search("\u{6f22}"), vec![((4, 0), (4, 1))]);
    assert_eq!(t.text_range(found[1].0, found[1].1), "needle");
}

#[test]
fn search_from_covers_new_output() {
    let mut t = term(10, 3, "needle\r\n");
    let found = t.search("needle");
    let scrolled = t.lines_scrolled();
    // A match that wraps from scrollback onto the screen, and one in new output.
    t.feed(b"1\r\n2\r\nxxxxxxneedle\r\nneedle");
    let (start, new) = t.search_from("needle", scrolled);
    let mut all: Vec<_> = found.into_iter().filter(|m| m.0.0 < start).collect();
    all.extend(new);
    assert_eq!(all, t.search("needle"));
    assert_eq!(all.len(), 3);
}

#[test]
fn osc8_links_cells_and_survives_sgr() {
    let t = term(
        40,
        2,
        "a \x1b]8;;https://example.com/x;y\x07li\x1b[1;31mnk\x1b[0m!\x1b]8;;\x07 b",
    );
    let id = t.link_at(0, 2).expect("linked");
    assert_eq!(t.link(id), Some("https://example.com/x;y"));
    // SGR changes and resets in the middle don't end the link.
    assert_eq!(t.link_at(0, 4), Some(id));
    assert_eq!(t.link_at(0, 6), Some(id));
    assert_eq!(t.link_at(0, 0), None);
    assert_eq!(t.link_at(0, 8), None);
    // Same id and target: one link, even split across lines; other ids differ.
    let t = term(
        40,
        3,
        "\x1b]8;id=a;https://x\x1b\\one\x1b]8;;\x1b\\\r\n\x1b]8;id=a;https://x\x1b\\two\x1b]8;id=b;https://x\x1b\\3",
    );
    assert_eq!(t.link_at(0, 0), t.link_at(1, 0));
    assert_ne!(t.link_at(1, 0), t.link_at(1, 3));
    // Too long a target is ignored.
    let long = format!("\x1b]8;;https://{}\x07x", "a".repeat(5000));
    assert_eq!(term(10, 1, &long).link_at(0, 0), None);
}

#[test]
fn link_ids_are_reclaimed() {
    let mut t = Term::with_scrollback(10, 1, 0);
    for i in 0..70_000 {
        t.feed(format!("\r\x1b]8;;https://x/{i}\x07a\x1b]8;;\x07").as_bytes());
    }
    let id = t.link_at(0, 0).expect("still linking");
    assert_eq!(t.link(id), Some("https://x/69999"));
}

#[test]
fn urls_are_found_in_plain_text() {
    let t = term(
        40,
        4,
        "see (https://example.com/a_(b)).\r\nmail me: mailto:me@x.org, ok\r\nxxxxxxxxxxxxhttps://wrap.example/long/path",
    );
    let (range, url) = t.url_at(0, 10).unwrap();
    assert_eq!(url, "https://example.com/a_(b)");
    assert_eq!(range, ((0, 5), (0, 29)));
    assert_eq!(t.url_at(0, 1), None);
    assert_eq!(t.url_at(1, 12).unwrap().1, "mailto:me@x.org");
    // Across a soft wrap, from either row.
    assert_eq!(t.url_at(3, 1).unwrap().1, "https://wrap.example/long/path");
    assert_eq!(t.url_at(2, 13).unwrap().0, ((2, 12), (3, 1)));
    // A scheme alone isn't a URL.
    assert_eq!(term(20, 1, "go to https:// now").url_at(0, 8), None);
}

#[test]
fn full_link_table_stays_consistent() {
    // Every id held by a live cell: collecting frees nothing, and the pen's
    // cached style must not survive the renumbering.
    let mut t = Term::with_scrollback(256, 256, 0);
    for i in 0..u16::MAX as usize + 10 {
        t.feed(format!("\x1b]8;;https://x/{i}\x07\x1b[3{}ma", i % 8).as_bytes());
    }
    t.feed(b"\x1b]8;;https://x/0\x07\x1b[31mb\x1b]8;;\x07c");
    let (row, col) = t.cursor();
    assert_eq!(t.lines()[row].cells[col - 1].c, 'c');
    assert_eq!(t.link_at(t.lines_scrolled() + row as u64, col - 1), None);
}

#[test]
fn notifications_and_bells() {
    let mut t = term(
        20,
        3,
        "\x1b]9;Build done; 3 warnings\x07\x07\x1b]777;notify;Claude;Waiting for you\x1b\\",
    );
    let n = t.take_notifications();
    assert_eq!(n.len(), 2);
    assert_eq!(
        (n[0].title.as_str(), n[0].body.as_str()),
        ("", "Build done; 3 warnings")
    );
    assert_eq!(
        (n[1].title.as_str(), n[1].body.as_str()),
        ("Claude", "Waiting for you")
    );
    assert!(t.take_notifications().is_empty());
    // The OSC's terminating BEL isn't a bell; the lone one is.
    assert_eq!(t.bells(), 1);
    assert!(t.take_bell());
    // ConEmu progress (OSC 9;4) isn't a notification; control characters are dropped.
    t.feed("\x1b]9;4;1;50\x07\x1b]9;a\u{85}b\x07".as_bytes());
    assert_eq!(
        t.take_notifications()
            .iter()
            .map(|n| n.body.as_str())
            .collect::<Vec<_>>(),
        ["ab"]
    );
}

#[test]
fn kitty_keyboard_flags() {
    let mut t = Term::new(20, 3);
    t.feed(b"\x1b[?u");
    assert_eq!(t.take_responses(), b"\x1b[?0u");
    // Push, then set, or and clear bits on the top entry.
    t.feed(b"\x1b[>1u");
    assert_eq!(t.key_flags(), KEY_DISAMBIGUATE);
    t.feed(b"\x1b[=10;2u");
    assert_eq!(t.key_flags(), 11);
    t.feed(b"\x1b[=2;3u");
    assert_eq!(t.key_flags(), 9);
    t.feed(b"\x1b[=31u\x1b[?u");
    assert_eq!(t.take_responses(), b"\x1b[?31u");
    // The alternate screen has its own stack.
    t.feed(b"\x1b[?1049h");
    assert_eq!(t.key_flags(), 0);
    t.feed(b"\x1b[>5u");
    assert_eq!(t.key_flags(), 5);
    t.feed(b"\x1b[?1049l");
    assert_eq!(t.key_flags(), 31);
    // Popping goes back through the pushes, and past them to 0.
    t.feed(b"\x1b[>3u\x1b[<u");
    assert_eq!(t.key_flags(), 31);
    t.feed(b"\x1b[<5u");
    assert_eq!(t.key_flags(), 0);
    // Too many pushes drop the oldest; RIS clears everything.
    for i in 1..=20u8 {
        t.feed(format!("\x1b[>{i}u").as_bytes());
    }
    t.feed(b"\x1b[<8u");
    assert_eq!(t.key_flags(), 0);
    t.feed(b"\x1b[>4u\x1bc");
    assert_eq!(t.key_flags(), 0);
}

#[test]
fn output_cant_stall_the_terminal() {
    let start = std::time::Instant::now();
    let mut t = Term::new(80, 24);
    // REP with the largest count repeats a screenful at most.
    t.feed(b"x\x1b[65535b");
    assert_eq!(t.cursor().0, 23);
    // Every id in use: new clusters are dropped, without rescanning
    // every cell for each.
    let mut combining = Vec::new();
    for i in 0..70_000u32 {
        let base = char::from_u32(0x4E00 + i % 20_000).unwrap();
        let mark = char::from_u32(0x300 + i / 20_000).unwrap();
        combining.extend(format!("{base}{mark}").bytes());
    }
    t.feed(&combining);
    assert!(start.elapsed().as_secs() < 10, "{:?}", start.elapsed());
}

#[test]
fn position_report_above_the_origin_region() {
    let mut t = Term::new(80, 24);
    t.feed(b"\x1b[?6h\x1b7\x1b[5;10r\x1b8\x1b[6n");
    assert_eq!(t.take_responses(), b"\x1b[1;1R");
}

#[test]
fn underline_styles_and_colours() {
    let mut t = Term::new(20, 2);
    t.feed(b"\x1b[4:3;58:2::255:0:0ma\x1b[58;5;4mb\x1b[59mc\x1b[21md\x1b[4:0me\x1b[4;4:9mf\x1b[24mg\x1b[4:5mh\x1b[0mi");
    let cells: Vec<_> = (0..9).map(|c| t.resolve(t.lines()[0].cells[c])).collect();
    let shape: Vec<_> = cells
        .iter()
        .map(|c| {
            (
                c.flags.contains(Flags::UNDERLINE),
                c.underline,
                c.underline_color,
            )
        })
        .collect();
    use Underline::*;
    assert_eq!(
        shape,
        [
            (true, Curly, Color::Rgb(255, 0, 0)),
            (true, Curly, Color::Indexed(4)),
            (true, Curly, Color::Default),
            (true, Double, Color::Default),
            (false, Single, Color::Default),
            // An unknown style keeps what was set.
            (true, Single, Color::Default),
            (false, Single, Color::Default),
            (true, Dashed, Color::Default),
            (false, Single, Color::Default),
        ]
    );
    // Off again, a cell is as blank as one never styled; the style
    // bits don't show in a cell's flags.
    assert_eq!(cells[1].flags.bits() & 0xF000, 0);
    assert_eq!(std::mem::size_of::<Style>(), 16);
    assert_eq!(t.lines()[0].cells[8].style(), StyleId::DEFAULT);
}
