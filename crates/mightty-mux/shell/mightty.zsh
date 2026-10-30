# mightty shell integration for zsh.
#
# Marks prompts, commands and exit codes (OSC 133), reports the working
# directory (OSC 7) and the command line (OSC 633;E), so the terminal knows
# where each command's output starts and ends. Loaded automatically in
# mightty panes; to use it elsewhere, source this file from ~/.zshrc.

(( ${+functions[_mightty_precmd]} )) && return

typeset -g _mightty_running=0

_mightty_precmd() {
  local ret=$?
  if (( _mightty_running )); then
    printf '\e]133;D;%d\a' "$ret"
    _mightty_running=0
  fi
  printf '\e]7;file://%s%s\a' "${HOST}" "${PWD// /%20}"
  printf '\e]133;A\a'
}

_mightty_preexec() {
  local cmd=${1//\\/\\\\}
  cmd=${cmd//;/\\x3b}
  cmd=${cmd//$'\n'/\\x0a}
  printf '\e]633;E;%s\a' "$cmd"
  printf '\e]133;C\a'
  _mightty_running=1
}

autoload -Uz add-zsh-hook
add-zsh-hook precmd _mightty_precmd
add-zsh-hook preexec _mightty_preexec
