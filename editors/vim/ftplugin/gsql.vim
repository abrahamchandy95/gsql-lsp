if exists('b:did_ftplugin')
  finish
endif
let b:did_ftplugin = 1

setlocal commentstring=//\ %s
setlocal comments=s1:/*,mb:*,ex:*/,://,:#

" The GSQL Style Guide: indent the body of a block by 4 spaces, with spaces instead of
" tabs. Change it in after/ftplugin/gsql.vim.
setlocal shiftwidth=4 softtabstop=4 expandtab

let b:undo_ftplugin = 'setlocal commentstring< comments< shiftwidth< softtabstop< expandtab<'
