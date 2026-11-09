" Kitty keyboard protocol check: nvim --clean -u scripts/keytest/keys.vim
" In normal mode each key below echoes its name; K, then any key, shows
" the name Neovim received. Esc leaves insert mode at once only with the
" protocol (otherwise it waits out ttimeoutlen for an Alt chord).
set ttimeoutlen=3000
nnoremap <C-i> <Cmd>echo "ctrl-i"<CR>
nnoremap <Tab> <Cmd>echo "tab"<CR>
nnoremap <C-m> <Cmd>echo "ctrl-m"<CR>
nnoremap <CR> <Cmd>echo "enter"<CR>
nnoremap <S-CR> <Cmd>echo "shift-enter"<CR>
nnoremap <C-CR> <Cmd>echo "ctrl-enter"<CR>
nnoremap <C-Tab> <Cmd>echo "ctrl-tab"<CR>
nnoremap <S-Space> <Cmd>echo "shift-space"<CR>
nnoremap <C-;> <Cmd>echo "ctrl-;"<CR>
nnoremap <C-1> <Cmd>echo "ctrl-1"<CR>
nnoremap <C-S-a> <Cmd>echo "ctrl-shift-a"<CR>
nnoremap K <Cmd>echo keytrans(getcharstr())<CR>
