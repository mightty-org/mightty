# mightty shell integration for fish (3.0 and later).
#
# Marks prompts, commands and exit codes (OSC 133), reports the working
# directory (OSC 7) and the command line (OSC 633;E), so the terminal knows
# where each command's output starts and ends. fish finds this file because
# mightty puts its directory first in XDG_DATA_DIRS; to use it elsewhere,
# source it from ~/.config/fish/config.fish.

status is-interactive; or exit 0
set -q _mightty_loaded; and exit 0
set -g _mightty_loaded 1

# Take our directory back out of XDG_DATA_DIRS, so programs started from
# this shell see the user's value.
if set -q MIGHTTY_FISH_XDG_DATA_DIR
    # One colon-separated string: fish splits only *PATH variables.
    set -l dirs (string split : -- "$XDG_DATA_DIRS" | string match -v -- $MIGHTTY_FISH_XDG_DATA_DIR)
    if test (count $dirs) -gt 0
        set -gx XDG_DATA_DIRS (string join : -- $dirs)
    else
        set -e XDG_DATA_DIRS
    end
    set -e MIGHTTY_FISH_XDG_DATA_DIR
end

# The nonce proves marks come from this shell, so keep it from child
# processes: a plain (unexported) variable from here on.
set -g _mightty_mark ''
if set -q MIGHTTY_NONCE
    set _mightty_mark ";nonce=$MIGHTTY_NONCE"
    set -e MIGHTTY_NONCE
end
set -g _mightty_running 0

function _mightty_prompt --on-event fish_prompt
    # Percent-encoded, control characters too, so nothing can split it or
    # end it early (showing the nonce); with the nonce, like marks.
    printf '\e]7;file://%s%s%s\a' $hostname (string escape --style=url -- $PWD) "$_mightty_mark"
    printf '\e]133;A%s\a' $_mightty_mark
end

function _mightty_preexec --on-event fish_preexec
    set -l cmd (string join \n -- $argv[1] | string replace -a '\\' '\\\\' | string replace -a ';' '\\x3b' | string replace -a \a '\\x07' | string replace -a \e '\\x1b' | string replace -a \x18 '\\x18' | string replace -a \x1a '\\x1a' | string join '\\x0a')
    printf '\e]633;E;%s%s\a' "$cmd" $_mightty_mark
    printf '\e]133;C%s\a' $_mightty_mark
    set -g _mightty_running 1
end

function _mightty_postexec --on-event fish_postexec
    set -l ret $status
    if test $_mightty_running = 1
        printf '\e]133;D;%d%s\a' $ret $_mightty_mark
        set -g _mightty_running 0
    end
end
