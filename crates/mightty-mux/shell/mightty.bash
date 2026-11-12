# mightty shell integration for bash (3.2 and later).
#
# Marks prompts, commands and exit codes (OSC 133), reports the working
# directory (OSC 7) and the command line (OSC 633;E), so the terminal knows
# where each command's output starts and ends. Loaded automatically in
# mightty panes; to use it elsewhere, source this file from ~/.bashrc.

[[ $- == *i* ]] || builtin return 0
[[ -n "${_mightty_loaded-}" ]] && builtin return 0
_mightty_loaded=1

_mightty_running=0
# Set by mightty's bootstrap; marks carry it so mightty can tell them from
# output that imitates them. Empty when sourced by hand.
_mightty_mark=${_mightty_nonce:+;nonce=$_mightty_nonce}

# Before each prompt: how the last command ended, where we are, and that a
# prompt starts. First in PROMPT_COMMAND, so $? is still the command's.
_mightty_precmd() {
  local ret=$?
  if (( _mightty_running )); then
    builtin printf '\e]133;D;%d%s\a' "$ret" "$_mightty_mark"
    _mightty_running=0
  fi
  # Percent-encoded so `;` can't split it; with the nonce, like marks. Not
  # with control characters, which could end it early and show the nonce.
  if [[ $PWD != *[[:cntrl:]]* ]]; then
    local dir=${PWD//\%/%25}
    dir=${dir//;/%3B}
    builtin printf '\e]7;file://%s%s%s\a' "$HOSTNAME" "${dir// /%20}" "$_mightty_mark"
  fi
  builtin printf '\e]133;A%s\a' "$_mightty_mark"
  builtin return $ret
}

# As a command starts: its command line, and that its output follows.
_mightty_preexec() {
  local cmd=${1//\\/\\\\}
  cmd=${cmd//;/\\x3b}
  cmd=${cmd//$'\n'/\\x0a}
  # Bytes that would end or cancel the sequence early.
  cmd=${cmd//$'\a'/\\x07}
  cmd=${cmd//$'\e'/\\x1b}
  cmd=${cmd//$'\x18'/\\x18}
  cmd=${cmd//$'\x1a'/\\x1a}
  builtin printf '\e]633;E;%s%s\a' "$cmd" "$_mightty_mark"
  builtin printf '\e]133;C%s\a' "$_mightty_mark"
  _mightty_running=1
}

if [[ -n "${bash_preexec_imported-}${__bp_imported-}" ]]; then
  # bash-preexec owns the DEBUG trap; use its hooks.
  precmd_functions=(_mightty_precmd "${precmd_functions[@]}")
  preexec_functions+=(_mightty_preexec)
else
  # The DEBUG trap runs before every simple command; the first one after a
  # prompt is what the user typed. `_mightty_at_prompt` is set last in
  # PROMPT_COMMAND, so PROMPT_COMMAND's own commands don't count.
  _mightty_at_prompt=0
  _mightty_ready() {
    _mightty_at_prompt=1
  }
  _mightty_debug() {
    (( _mightty_at_prompt )) || builtin return 0
    [[ -n "${COMP_LINE-}" ]] && builtin return 0 # completion, not a command
    _mightty_at_prompt=0
    # What was typed, from history: "  42  make test" → "make test".
    local line
    line=$(HISTTIMEFORMAT= builtin history 1)
    line=${line#"${line%%[![:space:]]*}"}
    line=${line#"${line%%[![:digit:]]*}"}
    line=${line#\*}
    line=${line#"${line%%[![:space:]]*}"}
    _mightty_preexec "${line:-$BASH_COMMAND}"
  }
  if [[ -z "$(builtin trap -p DEBUG)" ]]; then
    builtin trap '_mightty_debug' DEBUG
  fi
  if [[ "$(builtin declare -p PROMPT_COMMAND 2>/dev/null)" == "declare -a"* ]]; then
    # bash 5.1+ runs each element.
    PROMPT_COMMAND=(_mightty_precmd "${PROMPT_COMMAND[@]}" _mightty_ready)
  else
    _mightty_pc=$PROMPT_COMMAND
    while [[ "$_mightty_pc" == *[\;[:space:]] ]]; do
      _mightty_pc=${_mightty_pc%?}
    done
    PROMPT_COMMAND="_mightty_precmd${_mightty_pc:+;$_mightty_pc};_mightty_ready"
    builtin unset _mightty_pc
  fi
fi
