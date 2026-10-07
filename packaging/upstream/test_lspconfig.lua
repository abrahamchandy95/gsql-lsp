-- Headless check of the nvim-lspconfig draft and the mason name mapping.
--   nvim --headless --clean -u NONE -l packaging/upstream/test_lspconfig.lua
-- Env: LSPCONFIG (default: lazy.nvim's nvim-lspconfig checkout, used only to
-- diff against real configs), MASON_YAML_JSON (optional, package.yaml as JSON).
local root = vim.fs.dirname(vim.fs.normalize(debug.getinfo(1, 'S').source:sub(2)))
local lspconfig = vim.env.LSPCONFIG or vim.fn.expand('~/.local/share/nvim/lazy/nvim-lspconfig')

local function fail(msg)
  io.stderr:write('FAIL: ' .. msg .. '\n')
  vim.cmd('cquit 1')
end

-- The draft directory goes first, as if it had been merged into nvim-lspconfig.
vim.opt.rtp:prepend(root .. '/nvim-lspconfig')
vim.opt.rtp:append(lspconfig)

local cfg = vim.lsp.config.gsql_lsp
if not cfg then
  fail('vim.lsp.config.gsql_lsp did not resolve')
end
assert(vim.deep_equal(cfg.cmd, { 'gsql-lsp' }), 'cmd')
assert(vim.deep_equal(cfg.filetypes, { 'gsql' }), 'filetypes')
assert(vim.deep_equal(cfg.root_markers, { '.gsqlroot', '.git' }), 'root_markers')

-- Same key set as the real configs: nothing outside vim.lsp.Config fields.
local allowed = {}
for _, k in ipairs({
  'cmd', 'filetypes', 'root_markers', 'root_dir', 'settings', 'on_attach', 'on_init', 'before_init',
  'capabilities', 'init_options', 'workspace_required', 'get_language_id', 'single_file_support',
  'name', 'handlers', 'commands', 'reuse_client', 'on_exit', 'on_error', 'flags', 'cmd_cwd', 'cmd_env',
  'offset_encoding', 'trace', 'workspace_folders', 'detached', 'before_init',
}) do
  allowed[k] = true
end
for k in pairs(cfg) do
  if not allowed[k] then
    fail('unexpected key ' .. k)
  end
end

-- Same shape as the files in nvim-lspconfig/lsp: @brief header, @type line.
local src = table.concat(vim.fn.readfile(root .. '/nvim-lspconfig/lsp/gsql_lsp.lua'), '\n')
assert(src:match('^%-%-%-@brief\n'), 'missing ---@brief header')
assert(src:match('\n%-%-%-@type vim%.lsp%.Config\nreturn {'), 'missing ---@type vim.lsp.Config')
assert(not src:find('\t'), 'tabs')

-- The lsp/ file is picked up by vim.lsp.enable + a matching buffer.
vim.filetype.add({ extension = { gsql = 'gsql' } })
vim.lsp.enable('gsql_lsp')
assert(vim.lsp.is_enabled('gsql_lsp'), 'not enabled')

-- Mason: ensure_installed = { 'gsql_lsp' } works when the registry package has
-- neovim.lspconfig = 'gsql_lsp' (mason-lspconfig.mappings builds the map that way).
local json = vim.env.MASON_YAML_JSON
if json then
  local spec = vim.json.decode(table.concat(vim.fn.readfile(json), '\n'))
  assert(vim.tbl_get(spec, 'neovim', 'lspconfig') == 'gsql_lsp', 'mason neovim.lspconfig')
  assert(vim.lsp.config[vim.tbl_get(spec, 'neovim', 'lspconfig')], 'mason name has no lsp config')
end

print('OK nvim-lspconfig draft resolves: ' .. vim.inspect(cfg.cmd))
vim.cmd('quit')
