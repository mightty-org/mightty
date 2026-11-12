/*
 * mightty.h - C API for the mightty terminal core (libmightty).
 *
 * libmightty parses what a program writes to its terminal and keeps the
 * resulting screen state. Feed it output bytes; read back cells, cursor,
 * modes, scrollback and shell-integration marks. It performs no I/O (kitty
 * graphics images must come in the output, not by file or shared memory),
 * starts no threads and never calls back into you, so it embeds anywhere:
 * GUI front ends, test harnesses, multiplexers, agent tooling, WebAssembly.
 *
 * Conventions
 *   - Functions that can fail return mightty_result; MIGHTTY_OK is 0.
 *   - Buffers are caller-owned. Copy-out functions take (out, cap, len):
 *     *len is always set to the full size required. If cap is smaller,
 *     nothing is copied or consumed and MIGHTTY_ERR_BUFFER is returned, so
 *     calling with out = NULL, cap = 0 queries the size.
 *   - Text is UTF-8 with an explicit length; it is not NUL-terminated.
 *   - Structs that may grow begin with `size`, which the caller sets to
 *     sizeof(the struct). The library fills only the fields that fit, so
 *     programs built against older headers keep working, and sets `size`
 *     to the bytes it filled.
 *   - A mightty_term is not thread-safe. Synchronise access yourself.
 *   - Lines are addressed absolutely: screen row r is line
 *     (lines_scrolled + r) and scrollback holds lines first_line up to
 *     lines_scrolled - 1. A line keeps its number while output scrolls.
 *
 * Compatibility: MIGHTTY_ABI_VERSION changes only on incompatible changes.
 * Compare it with mightty_abi_version() when loading the library at runtime.
 */
#ifndef MIGHTTY_H
#define MIGHTTY_H

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

#define MIGHTTY_ABI_VERSION 1

typedef int32_t mightty_result;
#define MIGHTTY_OK 0
#define MIGHTTY_ERR_NULL 1     /* a required pointer was NULL */
#define MIGHTTY_ERR_INVALID 2  /* an argument was out of range */
#define MIGHTTY_ERR_BUFFER 3   /* output buffer too small; *len holds the size needed */
#define MIGHTTY_ERR_INTERNAL 4 /* internal error (a bug); the handle may be unusable */

typedef struct mightty_term mightty_term;

/* ---- Cells ---- */

#define MIGHTTY_CELL_BOLD (1u << 0)
#define MIGHTTY_CELL_DIM (1u << 1)
#define MIGHTTY_CELL_ITALIC (1u << 2)
#define MIGHTTY_CELL_UNDERLINE (1u << 3)
#define MIGHTTY_CELL_BLINK (1u << 4)
#define MIGHTTY_CELL_INVERSE (1u << 5)
#define MIGHTTY_CELL_HIDDEN (1u << 6)
#define MIGHTTY_CELL_STRIKE (1u << 7)
#define MIGHTTY_CELL_WIDE (1u << 8)         /* left half of a double-width character */
#define MIGHTTY_CELL_WIDE_SPACER (1u << 9)  /* right half; draw nothing */
#define MIGHTTY_CELL_WRAP_SPACER (1u << 10) /* padding left when a wide char wrapped */
#define MIGHTTY_CELL_GRAPHEME (1u << 11)    /* several code points (combining marks,
                                               joined emoji, a flag); codepoint is the first */

#define MIGHTTY_COLOR_DEFAULT 0 /* the theme's foreground or background */
#define MIGHTTY_COLOR_INDEXED 1 /* value is a palette index, 0-255 */
#define MIGHTTY_COLOR_RGB 2     /* value is 0x00RRGGBB */

typedef struct mightty_cell {
    uint32_t codepoint; /* Unicode scalar value; ' ' when blank */
    uint32_t fg;        /* interpreted according to fg_kind */
    uint32_t bg;
    uint16_t flags;     /* MIGHTTY_CELL_* */
    uint8_t fg_kind;    /* MIGHTTY_COLOR_* */
    uint8_t bg_kind;
} mightty_cell; /* 16 bytes; fixed layout */

/* ---- State ---- */

#define MIGHTTY_MOUSE_OFF 0
#define MIGHTTY_MOUSE_CLICK 1  /* mode 1000 */
#define MIGHTTY_MOUSE_DRAG 2   /* mode 1002 */
#define MIGHTTY_MOUSE_MOTION 3 /* mode 1003 */

typedef struct mightty_state {
    uint32_t size; /* set to sizeof(mightty_state) before calling */
    uint32_t cols;
    uint32_t rows;
    uint32_t cursor_col;
    uint32_t cursor_row;
    uint32_t scrollback_len;
    uint64_t lines_scrolled; /* absolute line number of screen row 0 */
    uint64_t first_line;     /* absolute line number of the oldest scrollback row */
    uint8_t cursor_visible;
    uint8_t alt_screen;
    uint8_t app_cursor;
    uint8_t app_keypad;
    uint8_t bracketed_paste;
    uint8_t focus_events;
    uint8_t mouse_mode; /* MIGHTTY_MOUSE_* */
    uint8_t mouse_sgr;
    uint8_t sync_output;
    uint8_t cursor_shape; /* DECSCUSR value; 0 is the default */
    uint8_t _reserved[6];
} mightty_state; /* 56 bytes in ABI version 1 */

/* ---- Shell integration (OSC 133) ---- */

#define MIGHTTY_MARK_PROMPT_START 0
#define MIGHTTY_MARK_COMMAND_START 1
#define MIGHTTY_MARK_OUTPUT_START 2
#define MIGHTTY_MARK_COMMAND_END 3

typedef struct mightty_mark {
    uint64_t line;  /* absolute line */
    uint32_t kind;  /* MIGHTTY_MARK_* */
    int32_t exit_code;
    uint8_t has_exit_code; /* COMMAND_END marks only, when the shell reported one */
    uint8_t _reserved[7];
} mightty_mark; /* 24 bytes; fixed layout */

/* ---- Library ---- */

uint32_t mightty_abi_version(void);
/* Library version, e.g. "0.1.0". Static and NUL-terminated. */
const char *mightty_version(void);

/* ---- Lifecycle ---- */

/* cols and rows are clamped to at least 1; more than 10000 is an error.
 * Returns NULL on failure. */
mightty_term *mightty_term_new(uint32_t cols, uint32_t rows, uint32_t scrollback_lines);
/* Ignores NULL. */
void mightty_term_free(mightty_term *term);

/* ---- Input ---- */

/* Feeds program output. Partial escape sequences and UTF-8 carry over between calls. */
mightty_result mightty_term_feed(mightty_term *term, const uint8_t *bytes, size_t len);
/* As mightty_term_new: more than 10000 cols or rows is MIGHTTY_ERR_INVALID. */
mightty_result mightty_term_resize(mightty_term *term, uint32_t cols, uint32_t rows);

/* ---- Output ---- */

/* Bytes the program expects back (replies to queries such as cursor
 * position). Write them to the program's input. Consumed on success. */
mightty_result mightty_term_take_responses(mightty_term *term, uint8_t *out, size_t cap, size_t *len);
/* Sets *rang to 1 if BEL was received since the last call, else 0. */
mightty_result mightty_term_take_bell(mightty_term *term, uint8_t *rang);

/* ---- Reading state ---- */

mightty_result mightty_term_get_state(const mightty_term *term, mightty_state *out);
/* Cells of an absolute line. *wrapped, if not NULL, is set to 1 when the
 * line continues on the next one (soft wrap). MIGHTTY_ERR_INVALID if the
 * line is not in scrollback or on screen. */
mightty_result mightty_term_read_line(const mightty_term *term, uint64_t line, mightty_cell *out, size_t cap,
                                      size_t *len, uint8_t *wrapped);
/* Text between two points, inclusive, in either order. Soft wraps are
 * joined; hard line ends become '\n'. */
mightty_result mightty_term_text_range(const mightty_term *term, uint64_t start_line, uint32_t start_col,
                                       uint64_t end_line, uint32_t end_col, char *out, size_t cap, size_t *len);
/* The visible screen as text, trailing blanks trimmed. */
mightty_result mightty_term_screen_text(const mightty_term *term, char *out, size_t cap, size_t *len);
/* Window title (OSC 0/2); empty if never set. */
mightty_result mightty_term_title(const mightty_term *term, char *out, size_t cap, size_t *len);
/* Working directory reported by the shell (OSC 7); empty if unknown. */
mightty_result mightty_term_cwd(const mightty_term *term, char *out, size_t cap, size_t *len);
mightty_result mightty_term_marks(const mightty_term *term, mightty_mark *out, size_t cap, size_t *len);

#ifdef __cplusplus
}
#endif

#endif /* MIGHTTY_H */
