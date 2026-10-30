# mightty bootstrap: zsh finds this file because mightty points ZDOTDIR here.
# Restore the user's ZDOTDIR so the rest of their startup files load as usual,
# run their .zshenv, then hook in shell integration for interactive shells.
if [[ -n "${MIGHTTY_ZDOTDIR+x}" ]]; then
  ZDOTDIR="$MIGHTTY_ZDOTDIR"
  unset MIGHTTY_ZDOTDIR
else
  unset ZDOTDIR
fi

if [[ -f "${ZDOTDIR:-$HOME}/.zshenv" ]]; then
  source "${ZDOTDIR:-$HOME}/.zshenv"
fi

if [[ -o interactive && -n "${MIGHTTY_SHELL_INTEGRATION_DIR:-}" ]]; then
  source "$MIGHTTY_SHELL_INTEGRATION_DIR/mightty.zsh"
fi
