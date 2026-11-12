/* Exercises libmightty through its public header, as an embedder would. */
#include "mightty.h"

#include <stdio.h>
#include <string.h>

_Static_assert(sizeof(mightty_cell) == 16, "mightty_cell layout");
_Static_assert(sizeof(mightty_state) == 56, "mightty_state layout");
_Static_assert(sizeof(mightty_mark) == 24, "mightty_mark layout");

static int failures = 0;

#define CHECK(cond)                                                     \
    do {                                                                \
        if (!(cond)) {                                                  \
            fprintf(stderr, "%s:%d: CHECK failed: %s\n", __FILE__, __LINE__, #cond); \
            failures++;                                                 \
        }                                                               \
    } while (0)

static void feed(mightty_term *t, const char *s) {
    CHECK(mightty_term_feed(t, (const uint8_t *)s, strlen(s)) == MIGHTTY_OK);
}

int main(void) {
    CHECK(mightty_abi_version() == MIGHTTY_ABI_VERSION);
    CHECK(strlen(mightty_version()) > 0);

    mightty_term *t = mightty_term_new(20, 5, 100);
    CHECK(t != NULL);
    feed(t, "\x1b]0;hello\x07\x1b[1;31mred\x1b[0m \x1b[38;2;1;2;3mx\x1b[0m\r\n\x1b]133;D;7\x07");

    mightty_state st;
    memset(&st, 0, sizeof st);
    st.size = sizeof st;
    CHECK(mightty_term_get_state(t, &st) == MIGHTTY_OK);
    CHECK(st.cols == 20 && st.rows == 5);
    CHECK(st.cursor_row == 1 && st.cursor_col == 0);
    CHECK(st.cursor_visible == 1 && st.alt_screen == 0);
    /* A caller built against a newer header learns how much was filled. */
    struct { mightty_state state; uint8_t newer[8]; } grown;
    memset(&grown, 0, sizeof grown);
    grown.state.size = sizeof grown;
    CHECK(mightty_term_get_state(t, &grown.state) == MIGHTTY_OK);
    CHECK(grown.state.size == sizeof(mightty_state) && grown.state.cols == 20);

    /* Cells, colours, flags. */
    mightty_cell cells[20];
    size_t n = 0;
    uint8_t wrapped = 9;
    CHECK(mightty_term_read_line(t, st.lines_scrolled, cells, 20, &n, &wrapped) == MIGHTTY_OK);
    CHECK(n == 20 && wrapped == 0);
    CHECK(cells[0].codepoint == 'r' && (cells[0].flags & MIGHTTY_CELL_BOLD));
    CHECK(cells[0].fg_kind == MIGHTTY_COLOR_INDEXED && cells[0].fg == 1);
    CHECK(cells[3].fg_kind == MIGHTTY_COLOR_DEFAULT && cells[3].flags == 0);
    CHECK(cells[4].fg_kind == MIGHTTY_COLOR_RGB && cells[4].fg == 0x010203);

    /* Size query first, then copy. */
    size_t len = 0;
    CHECK(mightty_term_title(t, NULL, 0, &len) == MIGHTTY_ERR_BUFFER && len == 5);
    char title[16];
    CHECK(mightty_term_title(t, title, sizeof title, &len) == MIGHTTY_OK);
    CHECK(len == 5 && memcmp(title, "hello", 5) == 0);

    /* Shell integration marks. */
    mightty_mark marks[4];
    CHECK(mightty_term_marks(t, marks, 4, &n) == MIGHTTY_OK && n == 1);
    CHECK(marks[0].kind == MIGHTTY_MARK_COMMAND_END && marks[0].has_exit_code && marks[0].exit_code == 7);
    CHECK(marks[0].line == st.lines_scrolled + 1);

    /* Query replies stay queued until the buffer is big enough. */
    feed(t, "\x1b[6n");
    uint8_t small[2], big[16];
    CHECK(mightty_term_take_responses(t, small, sizeof small, &len) == MIGHTTY_ERR_BUFFER && len == 6);
    CHECK(mightty_term_take_responses(t, big, sizeof big, &len) == MIGHTTY_OK);
    CHECK(len == 6 && memcmp(big, "\x1b[2;1R", 6) == 0);
    CHECK(mightty_term_take_responses(t, big, sizeof big, &len) == MIGHTTY_OK && len == 0);

    /* No I/O: images can't be read from files. */
    feed(t, "\x1b_Ga=t,t=f,f=100,i=1;L2V0Yy9ob3N0cw==\x1b\\");
    CHECK(mightty_term_take_responses(t, big, sizeof big, &len) == MIGHTTY_ERR_BUFFER);
    char reply[64];
    CHECK(mightty_term_take_responses(t, (uint8_t *)reply, sizeof reply, &len) == MIGHTTY_OK);
    CHECK(len > 12 && memcmp(reply, "\x1b_Gi=1;EINVAL", 12) == 0);
    /* Nor after a reset (RIS), which output can ask for. */
    mightty_term *r = mightty_term_new(20, 5, 100);
    feed(r, "\x1b" "c\x1b_Ga=t,t=f,f=100,i=1;L2V0Yy9ob3N0cw==\x1b\\");
    CHECK(mightty_term_take_responses(r, (uint8_t *)reply, sizeof reply, &len) == MIGHTTY_OK);
    CHECK(len > 12 && memcmp(reply, "\x1b_Gi=1;EINVAL", 12) == 0);
    mightty_term_free(r);

    /* Text extraction. */
    char text[64];
    CHECK(mightty_term_text_range(t, 0, 0, 0, 2, text, sizeof text, &len) == MIGHTTY_OK);
    CHECK(len == 3 && memcmp(text, "red", 3) == 0);
    CHECK(mightty_term_resize(t, 10, 3) == MIGHTTY_OK);
    CHECK(mightty_term_screen_text(t, text, sizeof text, &len) == MIGHTTY_OK);
    CHECK(len == 5 && memcmp(text, "red x", 5) == 0);

    /* Bell. */
    uint8_t rang = 0;
    feed(t, "\x07");
    CHECK(mightty_term_take_bell(t, &rang) == MIGHTTY_OK && rang == 1);
    CHECK(mightty_term_take_bell(t, &rang) == MIGHTTY_OK && rang == 0);

    /* A caller built against an older, smaller mightty_state gets only its prefix. */
    mightty_state old;
    memset(&old, 0xAB, sizeof old);
    old.size = 12; /* size, cols, rows */
    CHECK(mightty_term_get_state(t, &old) == MIGHTTY_OK);
    CHECK(old.size == 12 && old.cols == 10 && old.rows == 3);
    CHECK(old.cursor_col == 0xABABABABu);

    /* Errors. */
    CHECK(mightty_term_feed(NULL, (const uint8_t *)"x", 1) == MIGHTTY_ERR_NULL);
    CHECK(mightty_term_read_line(t, 999, cells, 20, &n, NULL) == MIGHTTY_ERR_INVALID);
    CHECK(mightty_term_title(t, title, sizeof title, NULL) == MIGHTTY_ERR_NULL);
    old.size = 0;
    CHECK(mightty_term_get_state(t, &old) == MIGHTTY_ERR_INVALID);

    mightty_term_free(t);
    mightty_term_free(NULL);

    if (failures) {
        fprintf(stderr, "%d check(s) failed\n", failures);
        return 1;
    }
    puts("ok");
    return 0;
}
