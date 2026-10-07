-- Makes the repository root a plugin for lazy.nvim and vim.pack, which install
-- the whole repository: the real plugin is in editors/neovim. This appends it
-- to 'runtimepath' and loads its module under the same name.
local dir = vim.fn.fnamemodify(debug.getinfo(1, 'S').source:sub(2), ':p:h:h:h') .. '/editors/neovim'
if not vim.list_contains(vim.tbl_map(vim.fs.normalize, vim.opt.rtp:get()), vim.fs.normalize(dir)) then
  vim.opt.rtp:append(dir)
end
return dofile(dir .. '/lua/gsql/init.lua')
