# mightty shell integration for zsh.
#
# Marks prompts, commands and exit codes (OSC 133), reports the working
# directory (OSC 7) and the command line (OSC 633;E), so the terminal knows
# where each command's output starts and ends. Loaded automatically in
# mightty panes; to use it elsewhere, source this file from ~/.zshrc.

(( ${+functions[_mightty_precmd]} )) && return

typeset -g _mightty_running=0
# Set by mightty's bootstrap; marks carry it so mightty can tell them from
# output that imitates them. Empty when sourced by hand.
typeset -g _mightty_mark=${_mightty_nonce:+;nonce=$_mightty_nonce}

_mightty_precmd() {
  local ret=$?
  if (( _mightty_running )); then
    printf '\e]133;D;%d%s\a' "$ret" "$_mightty_mark"
    _mightty_running=0
  fi
  # Percent-encoded so `;` can't split it; with the nonce, like marks. Not
  # with control characters, which could end it early and show the nonce.
  if [[ $PWD != *[[:cntrl:]]* ]]; then
    local dir=${PWD//\%/%25}
    dir=${dir//;/%3B}
    printf '\e]7;file://%s%s%s\a' "${HOST}" "${dir// /%20}" "$_mightty_mark"
  fi
  printf '\e]133;A%s\a' "$_mightty_mark"
}

_mightty_preexec() {
  local cmd=${1//\\/\\\\}
  cmd=${cmd//;/\\x3b}
  cmd=${cmd//$'\n'/\\x0a}
  # Bytes that would end or cancel the sequence early.
  cmd=${cmd//$'\a'/\\x07}
  cmd=${cmd//$'\e'/\\x1b}
  cmd=${cmd//$'\x18'/\\x18}
  cmd=${cmd//$'\x1a'/\\x1a}
  printf '\e]633;E;%s%s\a' "$cmd" "$_mightty_mark"
  printf '\e]133;C%s\a' "$_mightty_mark"
  _mightty_running=1
}

# Where typing begins, once the prompt is drawn: here rather than at the
# end of PS1, which prompt themes rebuild before every prompt.
_mightty_line_init() {
  printf '\e]133;B%s\a' "$_mightty_mark"
}

# The line editor isn't loaded yet while startup files run, so this hooks
# in at the first prompt.
_mightty_hook_zle() {
  add-zsh-hook -d precmd _mightty_hook_zle
  autoload -Uz add-zle-hook-widget && add-zle-hook-widget line-init _mightty_line_init
}

autoload -Uz add-zsh-hook
add-zsh-hook precmd _mightty_precmd
add-zsh-hook preexec _mightty_preexec
add-zsh-hook precmd _mightty_hook_zle

# Home and End as Mightty sends them, which zsh leaves unbound (macOS's
# /etc/zshrc binds only the application-mode forms). Startup files that
# run after this one can still bind them to something else.
() {
  local keymap seq widget
  for keymap in emacs viins vicmd; do
    for seq widget in '\e[H' beginning-of-line '\e[F' end-of-line; do
      [[ $(bindkey -M $keymap $seq) == *undefined-key ]] &&
        bindkey -M $keymap $seq $widget
    done
  done
}
