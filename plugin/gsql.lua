-- See lua/gsql/init.lua: a package manager sources only the repository root's
-- plugin/ and ftdetect/, so load those of editors/neovim from here.
local dir = vim.fn.fnamemodify(debug.getinfo(1, 'S').source:sub(2), ':p:h:h') .. '/editors/neovim'
require('gsql') -- puts the directory on 'runtimepath': ftplugin/, lsp/, queries/, parser/
dofile(dir .. '/ftdetect/gsql.lua')
dofile(dir .. '/plugin/gsql.lua')
