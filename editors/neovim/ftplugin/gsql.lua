if vim.b.did_ftplugin then
  return
end
vim.b.did_ftplugin = true

vim.bo.commentstring = '// %s'
vim.bo.comments = 's1:/*,mb:*,ex:*/,://,:#'

-- The GSQL Style Guide: indent the body of a block by 4 spaces, with spaces instead of
-- tabs. The formatter indents by 'shiftwidth', so typing and formatting agree. A
-- .editorconfig in the project, or your own after/ftplugin/gsql.lua, takes precedence.
local editorconfig = vim.b.editorconfig or {}
if editorconfig.indent_size == nil and editorconfig.indent_style == nil then
  vim.bo.shiftwidth = 4
  vim.bo.softtabstop = 4
  vim.bo.expandtab = true
end

-- Tree-sitter highlighting, when a gsql parser is installed (see :GsqlBuildParser).
pcall(vim.treesitter.start, 0, 'gsql')

vim.b.undo_ftplugin = 'setlocal commentstring< comments< shiftwidth< softtabstop< expandtab<'
