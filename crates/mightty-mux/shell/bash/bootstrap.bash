# mightty bootstrap: mightty starts bash with `--rcfile` pointing here, in
# place of ~/.bashrc. Run the startup files bash would have run, as a login
# shell when mightty asked for one, then hook in shell integration.

# The nonce proves marks come from this shell, so keep it from child
# processes: a plain (unexported) variable from here on.
_mightty_nonce=${MIGHTTY_NONCE-}
builtin unset MIGHTTY_NONCE

if [[ -n "${MIGHTTY_BASH_LOGIN-}" ]]; then
  builtin unset MIGHTTY_BASH_LOGIN
  if [[ -r /etc/profile ]]; then
    builtin source /etc/profile
  fi
  for _mightty_file in ~/.bash_profile ~/.bash_login ~/.profile; do
    if [[ -r "$_mightty_file" ]]; then
      builtin source "$_mightty_file"
      break
    fi
  done
  builtin unset _mightty_file
elif [[ -r ~/.bashrc ]]; then
  builtin source ~/.bashrc
fi

if [[ -n "${MIGHTTY_SHELL_INTEGRATION_DIR-}" ]]; then
  builtin source "$MIGHTTY_SHELL_INTEGRATION_DIR/mightty.bash"
fi
